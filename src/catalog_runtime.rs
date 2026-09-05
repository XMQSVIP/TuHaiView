//! One catalog owner: bounded controls, cooperative scanning, snapshots and all index writes.
use super::{
    BATCH_SIZE, MetadataUpdate, SUPPORTED, ScanStats, cache_key, init_db, load_cached_connection,
    modified_ns, open_connection,
};
use crate::{
    duplicates::HashUpdate,
    models::{CatalogSnapshot, ImageRecord, SortMode},
    performance, storage,
};
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, bounded};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Debug)]
pub enum CatalogEvent {
    Started {
        generation: u64,
    },
    Progress {
        generation: u64,
        visited_files: usize,
        supported_images: usize,
        reused: usize,
        inserted: usize,
        updated: usize,
    },
    Finished {
        generation: u64,
        total: usize,
        stats: ScanStats,
    },
    Changed {
        generation: u64,
    },
    Error {
        generation: u64,
        message: String,
    },
    Cleared {
        generation: u64,
    },
}

pub fn path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

#[derive(Default, Debug)]
pub struct CatalogChangeSet {
    paths: BTreeMap<String, PathBuf>,
    pub full_rescan: bool,
    first: Option<Instant>,
    last: Option<Instant>,
}
impl CatalogChangeSet {
    fn add(&mut self, paths: impl IntoIterator<Item = PathBuf>, full: bool) {
        let now = Instant::now();
        self.first.get_or_insert(now);
        self.last = Some(now);
        self.full_rescan |= full;
        if !self.full_rescan {
            for path in paths {
                self.paths.insert(path_key(&path), path);
                if self.paths.len() > 4096 {
                    self.full_rescan = true;
                    break;
                }
            }
        }
        if self.full_rescan {
            self.paths.clear();
        }
    }
    fn due(&self) -> bool {
        self.last
            .is_some_and(|last| last.elapsed() >= Duration::from_millis(700))
            || self
                .first
                .is_some_and(|first| first.elapsed() >= Duration::from_secs(2))
    }
    fn scopes(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = Vec::new();
        // Look up ancestors directly instead of comparing every pair of paths.
        for (key, path) in &self.paths {
            if key
                .rmatch_indices('\\')
                .any(|(i, _)| self.paths.contains_key(&key[..i]))
            {
                continue;
            }
            roots.push(path.clone());
        }
        roots
    }
}

struct Request {
    root: PathBuf,
    generation: u64,
}
#[derive(Default)]
struct Pending {
    request: Option<Request>,
    clear: bool,
    changes: CatalogChangeSet,
    metadata: HashMap<i64, MetadataUpdate>,
    hashes: HashMap<i64, HashUpdate>,
    snapshot: Option<Arc<CatalogSnapshot>>,
    progress: Option<CatalogEvent>,
    retired: Vec<Box<dyn Send>>,
}

pub struct CatalogService {
    pub rx: Receiver<CatalogEvent>,
    pending: Arc<Mutex<Pending>>,
    notify_tx: Sender<()>,
    generation: Arc<AtomicU64>,
    closed: Arc<AtomicBool>,
    root: Option<PathBuf>,
}

impl CatalogService {
    pub fn new(wakeup: Arc<dyn Fn() + Send + Sync>) -> Result<Self> {
        Self::with_database(None, wakeup)
    }
    fn with_database(db: Option<PathBuf>, wakeup: Arc<dyn Fn() + Send + Sync>) -> Result<Self> {
        let (tx, rx) = bounded(32);
        let (notify_tx, notify_rx) = bounded(1);
        let pending = Arc::new(Mutex::new(Pending::default()));
        let generation = Arc::new(AtomicU64::new(0));
        let closed = Arc::new(AtomicBool::new(false));
        let p = pending.clone();
        let g = generation.clone();
        let c = closed.clone();
        let n = notify_tx.clone();
        thread::Builder::new()
            .name("catalog-owner".into())
            .spawn(move || {
                let opened = db.map_or_else(storage::database_path, Ok).and_then(|db| {
                    init_db(&db)?;
                    open_connection(&db)
                });
                match opened {
                    Ok(conn) => Worker {
                        conn,
                        pending: p,
                        generation: g,
                        closed: c,
                        tx,
                        wakeup,
                        notify_tx: n,
                        records: BTreeMap::new(),
                        root: PathBuf::new(),
                        active_generation: 0,
                        revision: 0,
                        watcher: None,
                        scan: None,
                        dirty: false,
                        last_publish: Instant::now(),
                        last_progress: Instant::now(),
                        published: None,
                    }
                    .run(notify_rx),
                    Err(error) => {
                        let _ = tx.send(CatalogEvent::Error {
                            generation: g.load(Ordering::Acquire),
                            message: format!("数据库初始化失败：{error}"),
                        });
                        wakeup();
                    }
                }
            })?;
        Ok(Self {
            rx,
            pending,
            notify_tx,
            generation,
            closed,
            root: None,
        })
    }
    pub fn scan(&mut self, root: PathBuf, _sort: SortMode) -> u64 {
        let different = self
            .root
            .as_ref()
            .is_none_or(|old| path_key(old) != path_key(&root));
        // Each explicit request invalidates previous completion events; the watcher
        // itself remains attached when the root stays the same.
        let generation = self
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        self.root = Some(root.clone());
        let mut p = self.pending.lock();
        p.request = Some(Request { root, generation });
        if different {
            p.changes = CatalogChangeSet::default();
        }
        if let Some(snapshot) = p.snapshot.take() {
            p.retired.push(Box::new(snapshot));
        }
        drop(p);
        let _ = self.notify_tx.try_send(());
        generation
    }
    pub fn cancel_scan(&mut self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.pending.lock().request = None;
        let _ = self.notify_tx.try_send(());
    }
    pub fn take_snapshot(&self) -> Option<Arc<CatalogSnapshot>> {
        self.pending.lock().snapshot.take()
    }
    pub fn take_progress(&self) -> Option<CatalogEvent> {
        self.pending.lock().progress.take()
    }
    pub fn retire(&self, snapshot: Arc<CatalogSnapshot>) {
        let bytes = snapshot.records.len() * std::mem::size_of::<Arc<ImageRecord>>();
        crate::retirement::retire(snapshot, bytes);
    }
    pub fn retire_value<T: Send + 'static>(&self, value: T) {
        crate::retirement::retire(value, std::mem::size_of::<T>());
    }
    pub fn queue_metadata_update(&self, update: MetadataUpdate) {
        self.pending.lock().metadata.insert(update.id, update);
        let _ = self.notify_tx.try_send(());
    }
    pub fn queue_hash_updates(&self, updates: Vec<HashUpdate>) {
        let mut p = self.pending.lock();
        for update in updates {
            p.hashes.insert(update.id, update);
        }
        drop(p);
        let _ = self.notify_tx.try_send(());
    }
    pub fn queue_changes(&self, paths: impl IntoIterator<Item = PathBuf>) {
        let paths: Vec<_> = paths.into_iter().collect();
        if paths.is_empty() {
            return;
        }
        self.pending.lock().changes.add(paths, false);
        let _ = self.notify_tx.try_send(());
    }
    pub fn clear_database(&mut self) -> Result<()> {
        self.cancel_scan();
        self.pending.lock().clear = true;
        let _ = self.notify_tx.try_send(());
        Ok(())
    }
    pub fn data_dir(&self) -> Result<PathBuf> {
        storage::data_dir()
    }
}
impl Drop for CatalogService {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        let _ = self.notify_tx.try_send(());
    }
}

struct Scan {
    iter: Box<dyn Iterator<Item = std::result::Result<walkdir::DirEntry, walkdir::Error>> + Send>,
    unseen: HashSet<String>,
    pending: Vec<ImageRecord>,
    stats: ScanStats,
    started: Instant,
    last_flush: Instant,
    force: bool,
}
struct Worker {
    conn: Connection,
    pending: Arc<Mutex<Pending>>,
    generation: Arc<AtomicU64>,
    closed: Arc<AtomicBool>,
    tx: Sender<CatalogEvent>,
    wakeup: Arc<dyn Fn() + Send + Sync>,
    notify_tx: Sender<()>,
    records: BTreeMap<String, Arc<ImageRecord>>,
    root: PathBuf,
    active_generation: u64,
    revision: u64,
    watcher: Option<RecommendedWatcher>,
    scan: Option<Scan>,
    dirty: bool,
    last_publish: Instant,
    last_progress: Instant,
    published: Option<Arc<CatalogSnapshot>>,
}
impl Worker {
    fn current(&self) -> bool {
        !self.closed.load(Ordering::Acquire)
            && self.active_generation == self.generation.load(Ordering::Acquire)
    }
    fn event(&self, mut event: CatalogEvent) {
        let progress = matches!(event, CatalogEvent::Progress { .. });
        if progress {
            self.pending.lock().progress = Some(event);
            (self.wakeup)();
            return;
        }
        if matches!(event, CatalogEvent::Finished { .. }) {
            self.pending.lock().progress = None;
        }
        loop {
            if self.closed.load(Ordering::Acquire) {
                break;
            }
            match self.tx.send_timeout(event, Duration::from_millis(10)) {
                Ok(()) => {
                    (self.wakeup)();
                    break;
                }
                Err(crossbeam_channel::SendTimeoutError::Timeout(e)) => {
                    if progress || !self.current() {
                        break;
                    }
                    event = e;
                }
                Err(_) => break,
            }
        }
    }
    fn run(mut self, notify_rx: Receiver<()>) {
        while !self.closed.load(Ordering::Acquire) {
            let (request, clear, retired) = {
                let mut p = self.pending.lock();
                (
                    p.request.take(),
                    std::mem::take(&mut p.clear),
                    std::mem::take(&mut p.retired),
                )
            };
            drop(retired);
            if !self.current() {
                self.scan = None;
            }
            if clear {
                self.scan = None;
                self.records.clear();
                self.revision += 1;
                self.dirty = true;
                let result = self
                    .conn
                    .execute_batch("DELETE FROM images; PRAGMA wal_checkpoint(TRUNCATE); VACUUM;");
                if let Err(error) = result {
                    self.event(CatalogEvent::Error {
                        generation: self.generation.load(Ordering::Acquire),
                        message: format!("清理数据库失败：{error}"),
                    });
                } else {
                    self.event(CatalogEvent::Cleared {
                        generation: self.generation.load(Ordering::Acquire),
                    });
                }
            }
            if let Some(request) = request {
                if let Err(error) = self.open(request) {
                    self.event(CatalogEvent::Error {
                        generation: self.active_generation,
                        message: error.to_string(),
                    });
                }
            }
            if self.current() {
                if let Err(error) = self.flush_metadata() {
                    tracing::warn!(%error, "metadata write failed");
                }
                if self.scan.is_none() {
                    let changes = {
                        let mut p = self.pending.lock();
                        p.changes.due().then(|| std::mem::take(&mut p.changes))
                    };
                    if let Some(changes) = changes {
                        self.event(CatalogEvent::Changed {
                            generation: self.active_generation,
                        });
                        let scopes = if changes.full_rescan {
                            vec![self.root.clone()]
                        } else {
                            changes.scopes()
                        };
                        performance::sample("incremental_scopes", scopes.len() as f64);
                        self.start_scan(scopes, !changes.full_rescan);
                    }
                }
                if self.scan.is_some() {
                    if let Err(error) = self.step() {
                        self.scan = None;
                        self.event(CatalogEvent::Error {
                            generation: self.active_generation,
                            message: error.to_string(),
                        });
                    }
                }
                if self.dirty && self.last_publish.elapsed() >= Duration::from_millis(100) {
                    self.publish();
                }
            }
            let timeout = if self.scan.is_some() {
                Duration::ZERO
            } else {
                Duration::from_millis(50)
            };
            let _ = notify_rx.recv_timeout(timeout);
        }
    }
    fn open(&mut self, request: Request) -> Result<()> {
        self.scan = None;
        self.active_generation = request.generation;
        let different_root = path_key(&self.root) != path_key(&request.root);
        self.root = request.root;
        if different_root || self.watcher.is_none() {
            self.watcher.take();
            let pending = self.pending.clone();
            let wakeup = self.wakeup.clone();
            let notify_tx = self.notify_tx.clone();
            let watched_root = self.root.clone();
            let data = storage::data_dir().ok();
            let database = self.conn.path().map(PathBuf::from);
            self.watcher = Some(notify::recommended_watcher(
                move |event: notify::Result<Event>| {
                    let (paths, rescan) = match event {
                        Ok(e) if e.need_rescan() || !matches!(e.kind, EventKind::Access(_)) => {
                            let rescan = e.need_rescan() || e.paths.is_empty();
                            (
                                e.paths
                                    .into_iter()
                                    .filter(|p| {
                                        p.starts_with(&watched_root)
                                            && !data.as_ref().is_some_and(|d| p.starts_with(d))
                                            && !database.as_ref().is_some_and(|d| {
                                                let key = path_key(p);
                                                let db = path_key(d);
                                                key == db
                                                    || key == format!("{db}-wal")
                                                    || key == format!("{db}-shm")
                                            })
                                    })
                                    .collect::<Vec<_>>(),
                                rescan,
                            )
                        }
                        Err(_) => (vec![], true),
                        _ => return,
                    };
                    if !paths.is_empty() || rescan {
                        pending.lock().changes.add(paths, rescan);
                        let _ = notify_tx.try_send(());
                        wakeup();
                    }
                },
            )?);
            if let Err(error) = self
                .watcher
                .as_mut()
                .unwrap()
                .watch(&self.root, RecursiveMode::Recursive)
            {
                self.watcher.take();
                self.event(CatalogEvent::Error {
                    generation: self.active_generation,
                    message: format!("目录监控不可用，可用 F5 校验：{error}"),
                });
            }
        }
        self.event(CatalogEvent::Started {
            generation: self.active_generation,
        });
        let cached = load_cached_connection(&self.conn, &self.root)?;
        self.records = cached
            .into_iter()
            .map(|r| (path_key(&r.path), Arc::new(r)))
            .collect();
        self.revision = self.revision.wrapping_add(1);
        self.publish();
        self.start_scan(vec![self.root.clone()], false);
        Ok(())
    }
    fn start_scan(&mut self, scopes: Vec<PathBuf>, force: bool) {
        let root_key = path_key(&self.root);
        let prefix = root_key.clone() + "\\";
        let scopes = scopes
            .into_iter()
            .filter(|p| {
                let k = path_key(p);
                k == root_key || k.starts_with(&prefix)
            })
            .collect::<Vec<_>>();
        let mut unseen = HashSet::new();
        for scope in &scopes {
            let key = path_key(scope);
            if self.records.contains_key(&key) {
                unseen.insert(key.clone());
            }
            let prefix = key + "\\";
            unseen.extend(
                self.records
                    .range(prefix.clone()..)
                    .take_while(|(key, _)| key.starts_with(&prefix))
                    .map(|(key, _)| key.clone()),
            );
        }
        // NotFound scopes are confirmed removals; every other traversal failure preserves entries.
        let mut errors = 0;
        let scopes = scopes
            .into_iter()
            .filter(|scope| match fs::symlink_metadata(scope) {
                Ok(metadata) => !metadata.file_type().is_symlink(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(_) => {
                    errors += 1;
                    false
                }
            })
            .collect::<Vec<_>>();
        let iter = scopes
            .into_iter()
            .flat_map(|scope| walkdir::WalkDir::new(scope).follow_links(false).into_iter());
        self.scan = Some(Scan {
            iter: Box::new(iter),
            unseen,
            pending: Vec::with_capacity(BATCH_SIZE),
            stats: ScanStats {
                traversal_errors: errors,
                ..Default::default()
            },
            started: Instant::now(),
            last_flush: Instant::now(),
            force,
        });
    }
    fn step(&mut self) -> Result<()> {
        let Some(mut scan) = self.scan.take() else {
            return Ok(());
        };
        let step_start = Instant::now();
        let mut finished = false;
        for _ in 0..128 {
            if !self.current() {
                return Ok(());
            }
            let Some(entry) = scan.iter.next() else {
                finished = true;
                break;
            };
            let entry = match entry {
                Ok(e) => e,
                Err(_) => {
                    scan.stats.traversal_errors += 1;
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            scan.stats.visited_files += 1;
            let path = entry.path();
            let Some(format) = path
                .extension()
                .and_then(|s| s.to_str())
                .map(str::to_ascii_lowercase)
                .filter(|f| SUPPORTED.contains(&f.as_str()))
            else {
                continue;
            };
            scan.stats.supported_images += 1;
            let key = path_key(path);
            scan.unseen.remove(&key);
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let modified = modified_ns(&metadata);
            let existing = self.records.get(&key);
            let same = existing.is_some_and(|r| {
                r.size == metadata.len() && r.modified_ns == modified && r.format == format
            });
            if same && !scan.force {
                scan.stats.reused += 1;
                continue;
            }
            let mut thumbnail_key = cache_key(path, metadata.len(), modified);
            if same && scan.force {
                use sha2::{Digest, Sha256};
                thumbnail_key = format!(
                    "{:x}",
                    Sha256::digest(
                        format!(
                            "{thumbnail_key}-{}-{}",
                            self.revision,
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_nanos()
                        )
                        .as_bytes()
                    )
                );
            }
            let record = ImageRecord {
                id: existing.map_or(0, |r| r.id),
                path: path.to_owned(),
                relative_path: path
                    .strip_prefix(&self.root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .into_owned(),
                file_name: path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                size: metadata.len(),
                modified_ns: modified,
                width: None,
                height: None,
                format,
                thumbnail_key,
                content_hash: None,
            };
            if existing.is_some() {
                scan.stats.updated += 1;
            } else {
                scan.stats.inserted += 1;
            }
            scan.pending.push(record);
            if scan.pending.len() >= BATCH_SIZE || step_start.elapsed() >= Duration::from_millis(10)
            {
                break;
            }
        }
        if !scan.pending.is_empty()
            && (finished
                || scan.pending.len() >= BATCH_SIZE
                || scan.last_flush.elapsed() >= Duration::from_millis(100)
                || self.records.is_empty())
        {
            let start = Instant::now();
            self.upsert(&mut scan.pending)?;
            scan.stats.db_write_ms += start.elapsed().as_millis();
            scan.last_flush = Instant::now();
            if self.records.len() <= 128 {
                self.publish();
            }
        }
        if finished {
            if self.current() && scan.stats.traversal_errors == 0 && !scan.unseen.is_empty() {
                let tx = self.conn.transaction()?;
                {
                    let mut stmt =
                        tx.prepare_cached("DELETE FROM images WHERE id=?1 AND root=?2")?;
                    for key in &scan.unseen {
                        if self.closed.load(Ordering::Acquire)
                            || self.active_generation != self.generation.load(Ordering::Acquire)
                        {
                            return Ok(()); // Transaction rollback preserves the previous index.
                        }
                        if let Some(r) = self.records.get(key) {
                            stmt.execute(params![r.id, self.root.to_string_lossy()])?;
                        }
                    }
                }
                tx.commit()?;
                scan.stats.removed = scan.unseen.len();
                for key in scan.unseen {
                    self.records.remove(&key);
                }
                self.revision = self.revision.wrapping_add(1);
            }
            scan.stats.elapsed_ms = scan.started.elapsed().as_millis();
            performance::sample("scan_visited_files", scan.stats.visited_files as f64);
            performance::sample("scan_elapsed_ms", scan.stats.elapsed_ms as f64);
            self.publish();
            self.event(CatalogEvent::Finished {
                generation: self.active_generation,
                total: self.records.len(),
                stats: scan.stats,
            });
        } else {
            if self.last_progress.elapsed() >= Duration::from_millis(150) {
                self.event(CatalogEvent::Progress {
                    generation: self.active_generation,
                    visited_files: scan.stats.visited_files,
                    supported_images: scan.stats.supported_images,
                    reused: scan.stats.reused,
                    inserted: scan.stats.inserted,
                    updated: scan.stats.updated,
                });
                self.last_progress = Instant::now();
            }
            self.scan = Some(scan);
        }
        Ok(())
    }
    fn upsert(&mut self, records: &mut Vec<ImageRecord>) -> Result<()> {
        if !self.current() {
            records.clear();
            return Ok(());
        }
        super::upsert_records(&mut self.conn, &self.root, records)?;
        for record in records.drain(..) {
            self.records
                .insert(path_key(&record.path), Arc::new(record));
        }
        self.revision = self.revision.wrapping_add(1);
        self.dirty = true;
        Ok(())
    }
    fn publish(&mut self) {
        if !self.current() {
            return;
        }
        let start = Instant::now();
        let snapshot =
            Arc::new(
                if let Some(previous) = self.published.as_ref().filter(|p| {
                    p.generation == self.active_generation && p.revision == self.revision
                }) {
                    CatalogSnapshot {
                        records: self.records.values().cloned().collect::<Vec<_>>().into(),
                        generation: previous.generation,
                        revision: previous.revision,
                        by_path: previous.by_path.clone(),
                        by_id: previous.by_id.clone(),
                        natural_indices: previous.natural_indices.clone(),
                    }
                } else {
                    CatalogSnapshot::new(
                        self.active_generation,
                        self.revision,
                        self.records.values().cloned().collect(),
                    )
                },
            );
        self.published = Some(snapshot.clone());
        // Replacing an unconsumed snapshot also destroys it on this worker.
        let old = self.pending.lock().snapshot.replace(snapshot);
        drop(old);
        self.dirty = false;
        self.last_publish = Instant::now();
        (self.wakeup)();
        performance::elapsed("snapshot_build_ms", start);
    }
    fn flush_metadata(&mut self) -> Result<()> {
        let (metadata, hashes) = {
            let mut p = self.pending.lock();
            (
                std::mem::take(&mut p.metadata),
                std::mem::take(&mut p.hashes),
            )
        };
        if metadata.is_empty() && hashes.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        for update in metadata.into_values() {
            let Some(record) = self.records.get_mut(&path_key(&update.path)) else {
                continue;
            };
            if record.id != update.id
                || record.modified_ns != update.modified_ns
                || record.thumbnail_key != update.thumbnail_key
            {
                continue;
            }
            tx.execute(
                "UPDATE images SET width=?1,height=?2 WHERE id=?3 AND thumbnail_key=?4",
                params![update.width, update.height, update.id, update.thumbnail_key],
            )?;
            let record = Arc::make_mut(record);
            record.width = Some(update.width);
            record.height = Some(update.height);
            self.dirty = true;
        }
        for update in hashes.into_values() {
            let Some(record) = self.records.get_mut(&path_key(&update.path)) else {
                continue;
            };
            if record.id != update.id
                || record.size != update.size
                || record.modified_ns != update.modified_ns
                || record.thumbnail_key != update.thumbnail_key
            {
                continue;
            }
            tx.execute(
                "UPDATE images SET content_hash=?1 WHERE id=?2 AND thumbnail_key=?3",
                params![update.content_hash, update.id, update.thumbnail_key],
            )?;
            Arc::make_mut(record).content_hash = Some(update.content_hash);
            self.dirty = true;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn change_set_coalesces_subtrees_and_detects_overflow() {
        let mut set = CatalogChangeSet::default();
        set.add(
            [
                PathBuf::from("C:/a/sub"),
                PathBuf::from("c:/a/sub/one.jpg"),
                PathBuf::from("C:/a/two.jpg"),
            ],
            false,
        );
        assert_eq!(set.scopes().len(), 2);
        set.add(
            (0..5000).map(|n| PathBuf::from(format!("C:/p/{n}.jpg"))),
            false,
        );
        assert!(set.full_rescan);
        assert!(set.paths.is_empty());
    }
    #[test]
    fn actor_reuses_snapshot_and_applies_only_changed_paths() {
        let root = std::env::temp_dir().join(format!(
            "tuhai-runtime-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.jpg"), b"a").unwrap();
        fs::write(root.join("b.png"), b"b").unwrap();
        let mut service =
            CatalogService::with_database(Some(root.join("catalog.sqlite3")), Arc::new(|| {}))
                .unwrap();
        let generation = service.scan(root.clone(), SortMode::Path);
        let wait = |service: &CatalogService| {
            let until = Instant::now() + Duration::from_secs(10);
            loop {
                assert!(Instant::now() < until, "catalog did not finish");
                if let Ok(CatalogEvent::Finished { stats, .. }) =
                    service.rx.recv_timeout(Duration::from_millis(50))
                {
                    break stats;
                }
            }
        };
        wait(&service);
        let initial = service.take_snapshot().unwrap();
        assert_eq!(initial.records.len(), 2);
        fs::write(root.join("a.jpg"), b"changed").unwrap();
        service.queue_changes([root.join("a.jpg")]);
        let stats = wait(&service);
        assert_eq!(stats.visited_files, 1);
        assert_eq!(stats.updated, 1);
        let updated = service.take_snapshot().unwrap();
        assert_eq!(updated.generation, generation);
        let b1 = &initial.records[initial.by_path[&root.join("b.png")]];
        let b2 = &updated.records[updated.by_path[&root.join("b.png")]];
        assert!(Arc::ptr_eq(b1, b2));
        // Metadata-only publications keep the immutable lookup tables.
        service.queue_metadata_update(MetadataUpdate {
            id: b2.id,
            path: b2.path.clone(),
            modified_ns: b2.modified_ns,
            thumbnail_key: b2.thumbnail_key.clone(),
            width: 10,
            height: 20,
        });
        let until = Instant::now() + Duration::from_secs(5);
        let metadata_snapshot = loop {
            assert!(Instant::now() < until, "metadata was not published");
            if let Some(s) = service.take_snapshot() {
                if s.records[s.by_id[&b2.id]].width == Some(10) {
                    break s;
                }
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert!(Arc::ptr_eq(&updated.by_path, &metadata_snapshot.by_path));
        // A late hash/dimension result for the previous file version is ignored.
        let old = &initial.records[initial.by_path[&root.join("a.jpg")]];
        service.queue_metadata_update(MetadataUpdate {
            id: old.id,
            path: old.path.clone(),
            modified_ns: old.modified_ns,
            thumbnail_key: old.thumbnail_key.clone(),
            width: 999,
            height: 999,
        });
        fs::rename(root.join("a.jpg"), root.join("renamed.jpg")).unwrap();
        service.queue_changes([root.join("a.jpg"), root.join("renamed.jpg")]);
        wait(&service);
        let renamed = service.take_snapshot().unwrap();
        assert!(!renamed.by_path.contains_key(&root.join("a.jpg")));
        assert!(renamed.by_path.contains_key(&root.join("renamed.jpg")));
        assert!(renamed.records.iter().all(|r| r.width != Some(999)));
        fs::remove_file(root.join("b.png")).unwrap();
        service.queue_changes([root.join("b.png")]);
        wait(&service);
        assert_eq!(service.take_snapshot().unwrap().records.len(), 1);
        let next = service.scan(root.clone(), SortMode::Path);
        assert_ne!(next, generation);
        service.cancel_scan();
        let latest = service.scan(root.clone(), SortMode::Path);
        loop {
            if let CatalogEvent::Finished { generation: g, .. } =
                service.rx.recv_timeout(Duration::from_secs(10)).unwrap()
            {
                if g == latest {
                    break;
                }
            }
        }
        assert_eq!(service.take_snapshot().unwrap().generation, latest);
        let subtree = root.join("old-tree");
        fs::create_dir(&subtree).unwrap();
        for index in 0..10 {
            fs::write(subtree.join(format!("{index}.jpg")), b"new-image").unwrap();
        }
        service.queue_changes([subtree.clone()]);
        wait(&service);
        assert_eq!(service.take_snapshot().unwrap().records.len(), 11);
        let moved = root.join("new-tree");
        fs::rename(&subtree, &moved).unwrap();
        service.queue_changes([subtree.clone(), moved.clone()]);
        let move_stats = wait(&service);
        let after_move = service.take_snapshot().unwrap();
        assert_eq!(
            move_stats.visited_files, 10,
            "directory move must not traverse unrelated root files"
        );
        assert_eq!(after_move.records.len(), 11);
        assert!(
            after_move
                .records
                .iter()
                .all(|r| !r.path.starts_with(&subtree))
        );
        assert!(after_move.by_path.contains_key(&moved.join("0.jpg")));
        // Replacing a file invalidates progressive fields, even when timestamp/length are preserved.
        let replacement = moved.join("0.jpg");
        fs::write(&replacement, b"new-image").unwrap();
        service.queue_changes([replacement.clone()]);
        let replace_stats = wait(&service);
        assert_eq!(replace_stats.visited_files, 1);
        let replaced = service.take_snapshot().unwrap();
        assert_ne!(
            after_move.records[after_move.by_path[&replacement]].thumbnail_key,
            replaced.records[replaced.by_path[&replacement]].thumbnail_key
        );
        drop(service);
        // The owner exits asynchronously; wait only in the test for its DB/watcher handles.
        for _ in 0..100 {
            if fs::remove_dir_all(&root).is_ok() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("catalog worker did not close handles");
    }
}
