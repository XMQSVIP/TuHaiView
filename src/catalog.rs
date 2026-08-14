use crate::models::{ImageRecord, SortMode};
use crate::storage;
use anyhow::Result;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded, unbounded};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, UNIX_EPOCH},
};
use walkdir::WalkDir;

const SCHEMA_VERSION: i64 = 2;
const BATCH_SIZE: usize = 512;
const SUPPORTED: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "gif", "bmp", "tif", "tiff", "ico",
];

#[derive(Clone, Debug, Default)]
pub struct ScanStats {
    pub visited_files: usize,
    pub supported_images: usize,
    pub reused: usize,
    pub inserted: usize,
    pub updated: usize,
    pub removed: usize,
    pub traversal_errors: usize,
    pub elapsed_ms: u128,
    pub db_write_ms: u128,
}

#[derive(Clone, Debug)]
pub struct MetadataUpdate {
    pub id: i64,
    pub path: PathBuf,
    pub modified_ns: i64,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug)]
pub enum CatalogEvent {
    Started {
        generation: u64,
    },
    Snapshot {
        generation: u64,
        records: Vec<ImageRecord>,
    },
    UpsertBatch {
        generation: u64,
        records: Vec<ImageRecord>,
    },
    RemoveBatch {
        generation: u64,
        ids: Vec<i64>,
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
    Error {
        generation: u64,
        message: String,
    },
    Changed {
        generation: u64,
    },
}

pub struct CatalogService {
    tx: Sender<CatalogEvent>,
    pub rx: Receiver<CatalogEvent>,
    db_path: PathBuf,
    generation: u64,
    watcher: Option<RecommendedWatcher>,
    scan_cancel: Option<Arc<AtomicBool>>,
    scan_thread: Option<thread::JoinHandle<()>>,
    metadata_tx: Option<Sender<MetadataUpdate>>,
    metadata_thread: Option<thread::JoinHandle<()>>,
    wakeup: Arc<dyn Fn() + Send + Sync>,
}

impl CatalogService {
    pub fn new(wakeup: Arc<dyn Fn() + Send + Sync>) -> Result<Self> {
        let db_path = storage::database_path()?;
        let (tx, rx) = unbounded();
        let mut service = Self {
            tx,
            rx,
            db_path,
            generation: 0,
            watcher: None,
            scan_cancel: None,
            scan_thread: None,
            metadata_tx: None,
            metadata_thread: None,
            wakeup,
        };
        if let Err(error) = service.init_db() {
            // The index is generated data. If a previous version left a
            // partial migration, rebuild only the database and preserve the
            // original images and thumbnail cache.
            for suffix in ["", "-wal", "-shm"] {
                let path = if suffix.is_empty() {
                    service.db_path.clone()
                } else {
                    PathBuf::from(format!("{}{}", service.db_path.display(), suffix))
                };
                let _ = fs::remove_file(path);
            }
            service.init_db().map_err(|recovery| {
                anyhow::anyhow!("数据库初始化失败：{error}；重建失败：{recovery}")
            })?;
        }
        let (metadata_tx, metadata_rx) = bounded::<MetadataUpdate>(1024);
        let metadata_db = service.db_path.clone();
        let metadata_wakeup = service.wakeup.clone();
        service.metadata_thread = thread::Builder::new()
            .name("catalog-metadata-writer".into())
            .spawn(move || metadata_worker(&metadata_db, metadata_rx, metadata_wakeup))
            .ok();
        service.metadata_tx = Some(metadata_tx);
        Ok(service)
    }

    pub fn data_dir(&self) -> Result<PathBuf> {
        storage::data_dir()
    }

    pub fn clear_database(&mut self) -> Result<()> {
        self.cancel_scan();
        while self.rx.try_recv().is_ok() {}
        let conn = open_connection(&self.db_path)?;
        conn.execute("DELETE FROM images", [])?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
        Ok(())
    }

    fn init_db(&self) -> Result<()> {
        let conn = open_connection(&self.db_path)?;
        let has_table: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='images')",
            [],
            |row| row.get(0),
        )?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if !has_table {
            create_schema(&conn)?;
        } else if version < SCHEMA_VERSION {
            migrate_schema(&conn)?;
        }
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    }

    pub fn scan(&mut self, root: PathBuf, sort: SortMode) {
        self.cancel_scan();
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let tx = self.tx.clone();
        let db = self.db_path.clone();
        let wakeup = self.wakeup.clone();
        let _ = tx.send(CatalogEvent::Started { generation });
        wakeup();
        let root_for_scan = root.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let worker_wakeup = wakeup.clone();
        let error_wakeup = wakeup.clone();
        self.scan_cancel = Some(cancel);
        self.scan_thread = thread::Builder::new()
            .name("image-catalog-scanner".into())
            .spawn(move || {
                let result = scan_worker(
                    &root_for_scan,
                    &db,
                    generation,
                    &tx,
                    &worker_cancel,
                    sort,
                    worker_wakeup,
                );
                if let Err(error) = result {
                    let _ = tx.send(CatalogEvent::Error {
                        generation,
                        message: error.to_string(),
                    });
                    error_wakeup();
                }
            })
            .ok();

        let tx_watch = self.tx.clone();
        let root_watch = root;
        let generation_watch = generation;
        let watcher_wakeup = self.wakeup.clone();
        self.watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                if !matches!(event.kind, EventKind::Access(_)) {
                    let _ = tx_watch.send(CatalogEvent::Changed {
                        generation: generation_watch,
                    });
                    watcher_wakeup();
                }
            }
        })
        .ok();
        if let Some(watcher) = self.watcher.as_mut() {
            let _ =
                watcher.configure(Config::default().with_poll_interval(Duration::from_millis(500)));
            let _ = watcher.watch(&root_watch, RecursiveMode::Recursive);
        }
    }

    pub fn queue_metadata_update(&self, update: MetadataUpdate) {
        if self
            .metadata_tx
            .as_ref()
            .is_some_and(|sender| sender.try_send(update).is_ok())
        {
            (self.wakeup)();
        }
    }

    pub fn cancel_scan(&mut self) {
        self.watcher = None;
        if let Some(cancel) = self.scan_cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        if let Some(handle) = self.scan_thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for CatalogService {
    fn drop(&mut self) {
        self.cancel_scan();
        self.metadata_tx.take();
        if let Some(handle) = self.metadata_thread.take() {
            let _ = handle.join();
        }
    }
}

fn open_connection(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA temp_store=MEMORY;",
    )?;
    Ok(conn)
}

fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS images (
            id INTEGER PRIMARY KEY,
            root TEXT NOT NULL,
            path TEXT NOT NULL,
            relative_path TEXT NOT NULL,
            file_name TEXT NOT NULL,
            size INTEGER NOT NULL,
            modified_ns INTEGER NOT NULL,
            width INTEGER,
            height INTEGER,
            format TEXT NOT NULL,
            thumbnail_key TEXT NOT NULL,
            UNIQUE(root, path)
        );
        CREATE INDEX IF NOT EXISTS idx_images_root ON images(root);
        CREATE INDEX IF NOT EXISTS idx_images_root_modified ON images(root, modified_ns DESC);",
    )?;
    Ok(())
}

fn migrate_schema(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "DROP TABLE IF EXISTS images_v2;
        CREATE TABLE images_v2 (
            id INTEGER PRIMARY KEY,
            root TEXT NOT NULL,
            path TEXT NOT NULL,
            relative_path TEXT NOT NULL,
            file_name TEXT NOT NULL,
            size INTEGER NOT NULL,
            modified_ns INTEGER NOT NULL,
            width INTEGER,
            height INTEGER,
            format TEXT NOT NULL,
            thumbnail_key TEXT NOT NULL,
            UNIQUE(root, path)
        );
        INSERT OR REPLACE INTO images_v2
            (id, root, path, relative_path, file_name, size, modified_ns, width, height, format, thumbnail_key)
            SELECT id, root, path, relative_path, file_name, size, modified_ns, width, height, format, thumbnail_key
            FROM images;
        DROP TABLE images;
        ALTER TABLE images_v2 RENAME TO images;
        CREATE INDEX idx_images_root ON images(root);
        CREATE INDEX idx_images_root_modified ON images(root, modified_ns DESC);",
    )?;
    tx.commit()?;
    Ok(())
}

fn load_cached_connection(conn: &Connection, root: &Path) -> Result<Vec<ImageRecord>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id,path,relative_path,file_name,size,modified_ns,width,height,format,thumbnail_key
         FROM images WHERE root=?1",
    )?;
    let rows = stmt.query_map(params![root.to_string_lossy()], |row| {
        Ok(ImageRecord {
            id: row.get(0)?,
            path: PathBuf::from(row.get::<_, String>(1)?),
            relative_path: row.get(2)?,
            file_name: row.get(3)?,
            size: row.get::<_, i64>(4)? as u64,
            modified_ns: row.get(5)?,
            width: row.get(6)?,
            height: row.get(7)?,
            format: row.get(8)?,
            thumbnail_key: row.get(9)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn scan_worker(
    root: &Path,
    db_path: &Path,
    generation: u64,
    tx: &Sender<CatalogEvent>,
    cancel: &AtomicBool,
    sort: SortMode,
    wakeup: Arc<dyn Fn() + Send + Sync>,
) -> Result<()> {
    let started = Instant::now();
    let mut conn = open_connection(db_path)?;
    let snapshot = load_cached_connection(&conn, root)?;
    let mut cached = snapshot
        .iter()
        .cloned()
        .map(|record| (record.path.clone(), record))
        .collect::<HashMap<_, _>>();
    let mut sorted_snapshot = snapshot;
    sort_records(&mut sorted_snapshot, sort);
    let _ = tx.send(CatalogEvent::Snapshot {
        generation,
        records: sorted_snapshot,
    });
    wakeup();

    let mut seen = HashSet::<PathBuf>::with_capacity(cached.len());
    let mut pending = Vec::with_capacity(BATCH_SIZE);
    let mut stats = ScanStats::default();
    let mut last_progress = Instant::now();
    let mut db_write_time = Duration::ZERO;

    for entry_result in WalkDir::new(root).follow_links(false).into_iter() {
        if cancel.load(Ordering::Acquire) {
            return Ok(());
        }
        let entry = match entry_result {
            Ok(entry) => entry,
            Err(_) => {
                stats.traversal_errors += 1;
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        stats.visited_files += 1;
        let path = entry.path().to_path_buf();
        let format = match path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
        {
            Some(value) if SUPPORTED.contains(&value.as_str()) => value,
            _ => continue,
        };
        stats.supported_images += 1;
        let meta = match fs::metadata(&path) {
            Ok(meta) => meta,
            Err(_) => {
                if cached.contains_key(&path) {
                    seen.insert(path);
                }
                continue;
            }
        };
        let modified_ns = modified_ns(&meta);
        seen.insert(path.clone());
        if let Some(existing) = cached.get(&path)
            && existing.size == meta.len()
            && existing.modified_ns == modified_ns
            && existing.format == format
        {
            stats.reused += 1;
            continue;
        }
        let relative_path = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_owned();
        let is_update = cached.contains_key(&path);
        pending.push(ImageRecord {
            id: cached
                .get(&path)
                .map(|record| record.id)
                .unwrap_or_default(),
            path: path.clone(),
            relative_path,
            file_name,
            size: meta.len(),
            modified_ns,
            width: None,
            height: None,
            format,
            thumbnail_key: cache_key(&path, meta.len(), modified_ns),
        });
        if is_update {
            stats.updated += 1;
        } else {
            stats.inserted += 1;
        }
        cached.remove(&path);
        let batch_limit = if stats.inserted + stats.updated <= 128 {
            128
        } else {
            BATCH_SIZE
        };
        if pending.len() >= batch_limit {
            db_write_time += upsert_batch(&mut conn, root, &mut pending, generation, tx, &wakeup)?;
        }
        if last_progress.elapsed() >= Duration::from_millis(150) {
            send_progress(tx, generation, &stats);
            wakeup();
            last_progress = Instant::now();
        }
    }
    if cancel.load(Ordering::Acquire) {
        return Ok(());
    }
    if !pending.is_empty() {
        db_write_time += upsert_batch(&mut conn, root, &mut pending, generation, tx, &wakeup)?;
    }

    let removed_ids = if stats.traversal_errors == 0 {
        cached
            .values()
            .filter(|record| !seen.contains(&record.path))
            .map(|record| record.id)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    if !removed_ids.is_empty() {
        let started_delete = Instant::now();
        let transaction = conn.transaction()?;
        {
            let mut statement = transaction.prepare_cached("DELETE FROM images WHERE id=?1")?;
            for id in &removed_ids {
                statement.execute(params![id])?;
            }
        }
        transaction.commit()?;
        db_write_time += started_delete.elapsed();
        let _ = tx.send(CatalogEvent::RemoveBatch {
            generation,
            ids: removed_ids.clone(),
        });
        wakeup();
    }
    stats.removed = removed_ids.len();
    stats.db_write_ms = db_write_time.as_millis();
    stats.elapsed_ms = started.elapsed().as_millis();
    send_progress(tx, generation, &stats);
    let total = if stats.traversal_errors == 0 {
        stats.supported_images
    } else {
        stats.supported_images
            + cached
                .values()
                .filter(|record| !seen.contains(&record.path))
                .count()
    };
    let _ = tx.send(CatalogEvent::Finished {
        generation,
        total,
        stats,
    });
    wakeup();
    Ok(())
}

fn upsert_batch(
    conn: &mut Connection,
    root: &Path,
    pending: &mut Vec<ImageRecord>,
    generation: u64,
    tx: &Sender<CatalogEvent>,
    wakeup: &Arc<dyn Fn() + Send + Sync>,
) -> Result<Duration> {
    let started = Instant::now();
    let transaction = conn.transaction()?;
    {
        let mut statement = transaction.prepare_cached(
            "INSERT INTO images
                (root,path,relative_path,file_name,size,modified_ns,width,height,format,thumbnail_key)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(root,path) DO UPDATE SET
                relative_path=excluded.relative_path,
                file_name=excluded.file_name,
                size=excluded.size,
                modified_ns=excluded.modified_ns,
                width=excluded.width,
                height=excluded.height,
                format=excluded.format,
                thumbnail_key=excluded.thumbnail_key
             RETURNING id",
        )?;
        for record in pending.iter_mut() {
            record.id = statement.query_row(
                params![
                    root.to_string_lossy(),
                    record.path.to_string_lossy(),
                    record.relative_path,
                    record.file_name,
                    record.size as i64,
                    record.modified_ns,
                    record.width.map(|value| value as i64),
                    record.height.map(|value| value as i64),
                    record.format,
                    record.thumbnail_key,
                ],
                |row| row.get(0),
            )?;
        }
    }
    transaction.commit()?;
    let batch = std::mem::take(pending);
    let _ = tx.send(CatalogEvent::UpsertBatch {
        generation,
        records: batch,
    });
    wakeup();
    Ok(started.elapsed())
}

fn metadata_worker(
    db_path: &Path,
    rx: Receiver<MetadataUpdate>,
    wakeup: Arc<dyn Fn() + Send + Sync>,
) {
    let Ok(mut conn) = open_connection(db_path) else {
        return;
    };
    let mut pending = Vec::with_capacity(128);
    loop {
        let first = match rx.recv() {
            Ok(update) => update,
            Err(_) => {
                if !pending.is_empty() {
                    let _ = write_metadata_batch(&mut conn, &mut pending);
                }
                break;
            }
        };
        pending.push(first);
        let deadline = Instant::now() + Duration::from_millis(100);
        let mut disconnected = false;
        while pending.len() < 128 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok(update) => pending.push(update),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if write_metadata_batch(&mut conn, &mut pending).is_ok() {
            wakeup();
        }
        if disconnected {
            break;
        }
    }
}

fn write_metadata_batch(conn: &mut Connection, pending: &mut Vec<MetadataUpdate>) -> Result<()> {
    let transaction = conn.transaction()?;
    {
        let mut statement = transaction.prepare_cached(
            "UPDATE images SET width=?1,height=?2 WHERE id=?3 AND path=?4 AND modified_ns=?5",
        )?;
        for update in pending.iter() {
            statement.execute(params![
                update.width as i64,
                update.height as i64,
                update.id,
                update.path.to_string_lossy(),
                update.modified_ns,
            ])?;
        }
    }
    transaction.commit()?;
    pending.clear();
    Ok(())
}

fn send_progress(tx: &Sender<CatalogEvent>, generation: u64, stats: &ScanStats) {
    let _ = tx.send(CatalogEvent::Progress {
        generation,
        visited_files: stats.visited_files,
        supported_images: stats.supported_images,
        reused: stats.reused,
        inserted: stats.inserted,
        updated: stats.updated,
    });
}

fn modified_ns(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

pub fn cache_key(path: &Path, size: u64, modified_ns: i64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update(size.to_le_bytes());
    hasher.update(modified_ns.to_le_bytes());
    hasher.update(b"thumb-v2-256");
    format!("{:x}", hasher.finalize())
}

pub fn sort_records(records: &mut [ImageRecord], sort: SortMode) {
    match sort {
        SortMode::ModifiedDesc => records.sort_by(|a, b| b.modified_ns.cmp(&a.modified_ns)),
        SortMode::NameNatural => {
            records.sort_by(|a, b| natord::compare_ignore_case(&a.file_name, &b.file_name))
        }
        SortMode::SizeDesc => records.sort_by(|a, b| b.size.cmp(&a.size)),
        SortMode::Path => records.sort_by(|a, b| {
            a.relative_path
                .to_lowercase()
                .cmp(&b.relative_path.to_lowercase())
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use std::sync::atomic::AtomicBool;
    use std::time::SystemTime;

    fn record(name: &str) -> ImageRecord {
        ImageRecord {
            id: 0,
            path: PathBuf::new(),
            relative_path: name.into(),
            file_name: name.into(),
            size: 0,
            modified_ns: 0,
            width: None,
            height: None,
            format: "jpg".into(),
            thumbnail_key: name.into(),
        }
    }

    #[test]
    fn natural_sort_places_2_before_10() {
        let mut records = vec![
            record("image10.jpg"),
            record("image2.jpg"),
            record("image1.jpg"),
        ];
        sort_records(&mut records, SortMode::NameNatural);
        assert_eq!(
            records
                .iter()
                .map(|record| record.file_name.as_str())
                .collect::<Vec<_>>(),
            ["image1.jpg", "image2.jpg", "image10.jpg"]
        );
    }

    #[test]
    fn supported_extensions_are_case_insensitive() {
        assert!(SUPPORTED.contains(&"jpg"));
        assert!(SUPPORTED.contains(&"webp"));
        assert!(!SUPPORTED.contains(&"heic"));
    }

    #[test]
    fn cancelled_scan_stops_without_finished_event() {
        let root = std::env::temp_dir().join(format!("tuhai-view-cancel-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for index in 0..512 {
            fs::write(root.join(format!("image-{index}.jpg")), b"not-an-image").unwrap();
        }
        let db = root.join("catalog.sqlite3");
        let connection = Connection::open(&db).unwrap();
        create_schema(&connection).unwrap();
        drop(connection);
        let (tx, rx) = unbounded();
        let cancel = AtomicBool::new(true);
        let wakeup: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        scan_worker(&root, &db, 1, &tx, &cancel, SortMode::ModifiedDesc, wakeup).unwrap();
        assert!(
            rx.try_iter()
                .all(|event| !matches!(event, CatalogEvent::Finished { .. }))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn old_schema_migrates_and_allows_same_path_under_two_roots() {
        let db = std::env::temp_dir().join(format!("tuhai-view-migrate-{}", std::process::id()));
        let _ = fs::remove_file(&db);
        let connection = Connection::open(&db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE images (
                    id INTEGER PRIMARY KEY,
                    root TEXT NOT NULL,
                    path TEXT NOT NULL UNIQUE,
                    relative_path TEXT NOT NULL,
                    file_name TEXT NOT NULL,
                    size INTEGER NOT NULL,
                    modified_ns INTEGER NOT NULL,
                    width INTEGER,
                    height INTEGER,
                    format TEXT NOT NULL,
                    thumbnail_key TEXT NOT NULL
                );
                INSERT INTO images(root,path,relative_path,file_name,size,modified_ns,format,thumbnail_key)
                    VALUES ('C:/one','C:/one/a.jpg','a.jpg','a.jpg',1,1,'jpg','one');
                PRAGMA user_version=1;",
            )
            .unwrap();
        migrate_schema(&connection).unwrap();
        connection
            .execute(
                "INSERT INTO images(root,path,relative_path,file_name,size,modified_ns,format,thumbnail_key)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params!["C:/two", "C:/one/a.jpg", "a.jpg", "a.jpg", 1_i64, 1_i64, "jpg", "two"],
            )
            .unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM images", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
        drop(connection);
        let _ = fs::remove_file(db);
    }

    #[test]
    fn unchanged_incremental_scan_reuses_records_and_removes_missing_files() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("tuhai-view-incremental-{suffix}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.jpg"), b"a").unwrap();
        fs::write(root.join("b.png"), b"b").unwrap();
        let db = root.join("catalog.sqlite3");
        let connection = Connection::open(&db).unwrap();
        create_schema(&connection).unwrap();
        drop(connection);
        let wakeup: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        let cancel = AtomicBool::new(false);
        let (tx, rx) = unbounded();
        scan_worker(
            &root,
            &db,
            1,
            &tx,
            &cancel,
            SortMode::ModifiedDesc,
            wakeup.clone(),
        )
        .unwrap();
        let first = rx.try_iter().collect::<Vec<_>>();
        assert!(
            first
                .iter()
                .any(|event| matches!(event, CatalogEvent::UpsertBatch { .. }))
        );
        assert!(first.iter().any(
            |event| matches!(event, CatalogEvent::Finished { stats, .. } if stats.inserted == 2)
        ));

        let (tx, rx) = unbounded();
        scan_worker(
            &root,
            &db,
            2,
            &tx,
            &cancel,
            SortMode::ModifiedDesc,
            wakeup.clone(),
        )
        .unwrap();
        let second = rx.try_iter().collect::<Vec<_>>();
        assert!(
            !second
                .iter()
                .any(|event| matches!(event, CatalogEvent::UpsertBatch { .. }))
        );
        assert!(second.iter().any(
            |event| matches!(event, CatalogEvent::Finished { stats, .. } if stats.reused == 2)
        ));

        fs::remove_file(root.join("b.png")).unwrap();
        let (tx, rx) = unbounded();
        scan_worker(&root, &db, 3, &tx, &cancel, SortMode::ModifiedDesc, wakeup).unwrap();
        let third = rx.try_iter().collect::<Vec<_>>();
        assert!(
            third.iter().any(
                |event| matches!(event, CatalogEvent::RemoveBatch { ids, .. } if ids.len() == 1)
            )
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "manual release-mode performance benchmark"]
    fn batch_upsert_50k_completes_within_budget() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("catalog-perf-50k");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let db = root.join("catalog.sqlite3");
        let mut connection = open_connection(&db).unwrap();
        create_schema(&connection).unwrap();
        let (tx, _rx) = unbounded();
        let wakeup: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        let started = Instant::now();
        let mut pending = Vec::with_capacity(BATCH_SIZE);
        for index in 0..50_000 {
            let path = root.join(format!("image-{index}.jpg"));
            pending.push(ImageRecord {
                id: 0,
                relative_path: format!("image-{index}.jpg"),
                file_name: format!("image-{index}.jpg"),
                path: path.clone(),
                size: index as u64,
                modified_ns: index as i64,
                width: None,
                height: None,
                format: "jpg".into(),
                thumbnail_key: cache_key(&path, index as u64, index as i64),
            });
            if pending.len() == BATCH_SIZE {
                upsert_batch(&mut connection, &root, &mut pending, 1, &tx, &wakeup).unwrap();
            }
        }
        if !pending.is_empty() {
            upsert_batch(&mut connection, &root, &mut pending, 1, &tx, &wakeup).unwrap();
        }
        let elapsed = started.elapsed();
        eprintln!("50k batched upsert: {:.2}s", elapsed.as_secs_f64());
        assert!(elapsed < Duration::from_secs(15));
        drop(connection);
        fs::remove_dir_all(root).unwrap();
    }
}
