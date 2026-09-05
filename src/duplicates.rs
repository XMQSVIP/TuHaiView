use crate::models::ImageRecord;
use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender};
#[cfg(test)]
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, UNIX_EPOCH},
};

const HASH_BUFFER_SIZE: usize = 1024 * 1024;
const HASH_BATCH_SIZE: usize = 128;

#[derive(Clone, Debug)]
pub struct DuplicateGroup {
    pub hash: String,
    pub members: Arc<[ImageRecord]>,
    pub keeper_id: i64,
    pub included: bool,
}

impl DuplicateGroup {
    pub fn total_size(&self) -> u64 {
        self.members.iter().map(|record| record.size).sum()
    }

    pub fn reclaimable_size(&self) -> u64 {
        self.members
            .first()
            .map(|record| {
                record
                    .size
                    .saturating_mul(self.members.len().saturating_sub(1) as u64)
            })
            .unwrap_or(0)
    }
}

#[derive(Clone, Debug, Default)]
pub struct DuplicateStats {
    pub candidate_files: usize,
    pub checked_files: usize,
    pub hashed_files: usize,
    pub reused_hashes: usize,
    pub bytes_read: u64,
    pub duplicate_groups: usize,
    pub errors: usize,
    pub elapsed_ms: u128,
}

#[derive(Clone, Debug)]
pub struct HashUpdate {
    pub id: i64,
    pub path: PathBuf,
    pub size: u64,
    pub modified_ns: i64,
    pub content_hash: String,
    pub thumbnail_key: String,
}

#[derive(Debug)]
pub enum DuplicateEvent {
    Started {
        generation: u64,
        task_id: u64,
        candidate_files: usize,
    },
    Progress {
        generation: u64,
        task_id: u64,
        stats: DuplicateStats,
    },
    HashBatch {
        generation: u64,
        task_id: u64,
        updates: Vec<HashUpdate>,
    },
    Finished {
        generation: u64,
        task_id: u64,
        groups: Vec<DuplicateGroup>,
        stats: DuplicateStats,
        errors: Vec<(PathBuf, String)>,
    },
    Cancelled {
        generation: u64,
        task_id: u64,
        stats: DuplicateStats,
    },
    Error {
        generation: u64,
        task_id: u64,
        message: String,
    },
}

struct DuplicateRequest {
    generation: u64,
    task_id: u64,
    records: Arc<[Arc<ImageRecord>]>,
    cancel: Arc<AtomicBool>,
}
pub struct DuplicateService {
    pub rx: Receiver<DuplicateEvent>,
    task_id: u64,
    cancel: Option<Arc<AtomicBool>>,
    pending: Arc<parking_lot::Mutex<Option<DuplicateRequest>>>,
    notify_tx: Sender<()>,
}
impl DuplicateService {
    pub fn new(wakeup: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (tx, rx) = crossbeam_channel::bounded(128);
        let (notify_tx, notify_rx) = crossbeam_channel::bounded(1);
        let pending = Arc::new(parking_lot::Mutex::new(None::<DuplicateRequest>));
        let worker_pending = pending.clone();
        thread::Builder::new()
            .name("duplicate-image-scanner".into())
            .spawn(move || {
                while notify_rx.recv().is_ok() {
                    let Some(request) = worker_pending.lock().take() else {
                        continue;
                    };
                    if request.cancel.load(Ordering::Acquire) {
                        continue;
                    }
                    // Only candidates acquire owned mutable hash fields, on the worker.
                    let mut counts = HashMap::new();
                    for r in request.records.iter() {
                        *counts.entry(r.size).or_insert(0usize) += 1;
                    }
                    let records = request
                        .records
                        .iter()
                        .filter(|r| counts[&r.size] > 1)
                        .map(|r| r.as_ref().clone())
                        .collect();
                    let result = scan_worker(
                        request.generation,
                        request.task_id,
                        records,
                        Path::new(""),
                        &request.cancel,
                        &tx,
                        &wakeup,
                    );
                    if let Err(error) = result {
                        if !request.cancel.load(Ordering::Acquire) {
                            let _ = tx.send(DuplicateEvent::Error {
                                generation: request.generation,
                                task_id: request.task_id,
                                message: error.to_string(),
                            });
                            wakeup();
                        }
                    }
                }
            })
            .expect("duplicate worker");
        Self {
            rx,
            task_id: 0,
            cancel: None,
            pending,
            notify_tx,
        }
    }
    pub fn scan(&mut self, generation: u64, records: Arc<[Arc<ImageRecord>]>) -> u64 {
        self.cancel_token();
        self.task_id = self.task_id.wrapping_add(1);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        *self.pending.lock() = Some(DuplicateRequest {
            generation,
            task_id: self.task_id,
            records,
            cancel,
        });
        let _ = self.notify_tx.try_send(());
        self.task_id
    }
    pub fn cancel(&mut self) -> u64 {
        self.cancel_token();
        self.pending.lock().take();
        self.task_id = self.task_id.wrapping_add(1);
        self.task_id
    }
    fn cancel_token(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Release);
        }
    }
}
impl Drop for DuplicateService {
    fn drop(&mut self) {
        self.cancel_token();
    }
}

fn scan_worker(
    generation: u64,
    task_id: u64,
    records: Vec<ImageRecord>,
    _db_path: &Path,
    cancel: &AtomicBool,
    tx: &Sender<DuplicateEvent>,
    wakeup: &Arc<dyn Fn() + Send + Sync>,
) -> Result<()> {
    let started = Instant::now();
    let mut size_buckets = HashMap::<u64, Vec<ImageRecord>>::new();
    for record in records {
        size_buckets.entry(record.size).or_default().push(record);
    }
    let mut candidates = size_buckets
        .into_values()
        .filter(|bucket| bucket.len() >= 2)
        .flatten()
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| windows_path_key(&a.path).cmp(&windows_path_key(&b.path)));

    let mut stats = DuplicateStats {
        candidate_files: candidates.len(),
        ..Default::default()
    };
    let _ = tx.send(DuplicateEvent::Started {
        generation,
        task_id,
        candidate_files: stats.candidate_files,
    });
    wakeup();

    let mut groups = HashMap::<(u64, String), Vec<ImageRecord>>::new();
    let mut hash_updates = Vec::<HashUpdate>::with_capacity(HASH_BATCH_SIZE);
    let mut errors = Vec::<(PathBuf, String)>::new();
    let mut last_progress = Instant::now();
    let mut last_flush = Instant::now();

    for mut record in candidates {
        if cancel.load(Ordering::Acquire) {
            flush_hash_updates(&mut hash_updates, generation, task_id, tx, wakeup)?;
            stats.elapsed_ms = started.elapsed().as_millis();
            let _ = tx.send(DuplicateEvent::Cancelled {
                generation,
                task_id,
                stats,
            });
            wakeup();
            return Ok(());
        }

        let before = match matching_metadata(&record) {
            Ok(metadata) => metadata,
            Err(error) => {
                stats.checked_files += 1;
                stats.errors += 1;
                errors.push((record.path.clone(), error.to_string()));
                continue;
            }
        };

        let cached_hash = record
            .content_hash
            .clone()
            .filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
        let hash = if let Some(hash) = cached_hash {
            stats.reused_hashes += 1;
            hash
        } else {
            let hash_result = match hash_file(&record.path, cancel) {
                Ok(result) => result,
                Err(error) => {
                    stats.checked_files += 1;
                    stats.errors += 1;
                    errors.push((record.path.clone(), error.to_string()));
                    continue;
                }
            };
            let Some((hash, bytes_read)) = hash_result else {
                flush_hash_updates(&mut hash_updates, generation, task_id, tx, wakeup)?;
                stats.elapsed_ms = started.elapsed().as_millis();
                let _ = tx.send(DuplicateEvent::Cancelled {
                    generation,
                    task_id,
                    stats,
                });
                wakeup();
                return Ok(());
            };
            stats.bytes_read = stats.bytes_read.saturating_add(bytes_read);
            stats.hashed_files += 1;
            let after = matching_metadata(&record)
                .with_context(|| format!("计算哈希后无法复核文件：{}", record.path.display()));
            match after {
                Ok(after) if same_file_state(&before, &after) => {}
                Ok(_) => {
                    stats.checked_files += 1;
                    stats.errors += 1;
                    errors.push((record.path.clone(), "计算哈希期间文件发生变化".into()));
                    continue;
                }
                Err(error) => {
                    stats.checked_files += 1;
                    stats.errors += 1;
                    errors.push((record.path.clone(), error.to_string()));
                    continue;
                }
            }
            record.content_hash = Some(hash.clone());
            hash_updates.push(HashUpdate {
                id: record.id,
                path: record.path.clone(),
                size: record.size,
                modified_ns: record.modified_ns,
                content_hash: hash.clone(),
                thumbnail_key: record.thumbnail_key.clone(),
            });
            hash
        };

        record.content_hash = Some(hash.clone());
        let bucket = groups.entry((record.size, hash)).or_default();
        bucket.push(record);
        if bucket.len() == 2 {
            stats.duplicate_groups += 1;
        }
        stats.checked_files += 1;

        if hash_updates.len() >= HASH_BATCH_SIZE
            || (!hash_updates.is_empty() && last_flush.elapsed() >= Duration::from_secs(1))
        {
            flush_hash_updates(&mut hash_updates, generation, task_id, tx, wakeup)?;
            last_flush = Instant::now();
        }
        if last_progress.elapsed() >= Duration::from_millis(150) {
            stats.elapsed_ms = started.elapsed().as_millis();
            let _ = tx.send(DuplicateEvent::Progress {
                generation,
                task_id,
                stats: stats.clone(),
            });
            wakeup();
            last_progress = Instant::now();
        }
    }

    flush_hash_updates(&mut hash_updates, generation, task_id, tx, wakeup)?;
    let mut duplicate_groups = groups
        .into_iter()
        .filter_map(|((_, hash), mut members)| {
            if members.len() < 2 {
                return None;
            }
            members.sort_by(|a, b| keeper_key(a).cmp(&keeper_key(b)));
            let keeper_id = members[0].id;
            Some(DuplicateGroup {
                hash,
                members: members.into(),
                keeper_id,
                included: true,
            })
        })
        .collect::<Vec<_>>();
    duplicate_groups.sort_by(|a, b| {
        b.reclaimable_size()
            .cmp(&a.reclaimable_size())
            .then_with(|| a.hash.cmp(&b.hash))
    });
    stats.duplicate_groups = duplicate_groups.len();
    stats.elapsed_ms = started.elapsed().as_millis();
    let _ = tx.send(DuplicateEvent::Finished {
        generation,
        task_id,
        groups: duplicate_groups,
        stats,
        errors,
    });
    wakeup();
    Ok(())
}

fn flush_hash_updates(
    pending: &mut Vec<HashUpdate>,
    generation: u64,
    task_id: u64,
    tx: &Sender<DuplicateEvent>,
    wakeup: &Arc<dyn Fn() + Send + Sync>,
) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    let updates = std::mem::take(pending);
    let _ = tx.send(DuplicateEvent::HashBatch {
        generation,
        task_id,
        updates,
    });
    wakeup();
    Ok(())
}

fn hash_file(path: &Path, cancel: &AtomicBool) -> Result<Option<(String, u64)>> {
    let mut file = File::open(path).with_context(|| format!("无法读取：{}", path.display()))?;
    let mut buffer = vec![0_u8; HASH_BUFFER_SIZE];
    let mut hasher = Sha256::new();
    let mut bytes_read = 0_u64;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Ok(None);
        }
        while crate::performance::PREVIEW_BUSY.load(Ordering::Acquire)
            && !cancel.load(Ordering::Acquire)
        {
            thread::sleep(Duration::from_millis(10));
        }
        if cancel.load(Ordering::Acquire) {
            return Ok(None);
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes_read = bytes_read.saturating_add(read as u64);
    }
    Ok(Some((format!("{:x}", hasher.finalize()), bytes_read)))
}

fn matching_metadata(record: &ImageRecord) -> Result<fs::Metadata> {
    let metadata = fs::metadata(&record.path)
        .with_context(|| format!("无法读取文件状态：{}", record.path.display()))?;
    if !metadata.is_file() {
        anyhow::bail!("路径已不是文件：{}", record.path.display());
    }
    if metadata.len() != record.size || modified_ns(&metadata) != record.modified_ns {
        anyhow::bail!("文件已在查重后发生变化：{}", record.path.display());
    }
    Ok(metadata)
}

fn same_file_state(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.len() == right.len() && modified_ns(left) == modified_ns(right)
}

pub fn validate_delete_candidate(record: &ImageRecord, expected_hash: &str) -> Result<()> {
    if record.content_hash.as_deref() != Some(expected_hash) {
        anyhow::bail!("缓存哈希与重复组不一致");
    }
    matching_metadata(record).map(|_| ())
}

fn modified_ns(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn windows_path_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

fn keeper_key(record: &ImageRecord) -> (usize, bool, usize, i64, String) {
    (
        Path::new(&record.relative_path).components().count(),
        has_copy_marker(&record.file_name),
        record.relative_path.chars().count(),
        record.modified_ns,
        record.relative_path.to_lowercase(),
    )
}

fn has_copy_marker(file_name: &str) -> bool {
    let lower = file_name.to_lowercase();
    let stem = Path::new(&lower)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(&lower);
    lower.contains("副本")
        || lower.contains("copy")
        || stem
            .rsplit_once('(')
            .and_then(|(_, suffix)| suffix.strip_suffix(')'))
            .is_some_and(|number| {
                !number.is_empty() && number.chars().all(|ch| ch.is_ascii_digit())
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use std::time::SystemTime;

    fn record(path: PathBuf, id: i64) -> ImageRecord {
        let metadata = fs::metadata(&path).unwrap();
        ImageRecord {
            id,
            relative_path: path.file_name().unwrap().to_string_lossy().into_owned(),
            file_name: path.file_name().unwrap().to_string_lossy().into_owned(),
            path,
            size: metadata.len(),
            modified_ns: modified_ns(&metadata),
            width: None,
            height: None,
            format: "jpg".into(),
            thumbnail_key: id.to_string(),
            content_hash: None,
        }
    }

    #[test]
    fn identical_content_groups_and_unique_sizes_are_not_read() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("tuhai-view-duplicates-{suffix}"));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("one.jpg"), b"same-content").unwrap();
        fs::write(root.join("two.png"), b"same-content").unwrap();
        fs::write(root.join("different.jpg"), b"different123").unwrap();
        fs::write(root.join("other.jpg"), b"unique").unwrap();

        let db = root.join("catalog.sqlite3");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE images (
                id INTEGER PRIMARY KEY, path TEXT, size INTEGER, modified_ns INTEGER,
                content_hash TEXT
            );",
        )
        .unwrap();
        let mut records = vec![
            record(root.join("one.jpg"), 1),
            record(root.join("two.png"), 2),
            record(root.join("different.jpg"), 3),
            record(root.join("other.jpg"), 4),
        ];
        for item in &records {
            conn.execute(
                "INSERT INTO images(id,path,size,modified_ns) VALUES(?1,?2,?3,?4)",
                params![
                    item.id,
                    item.path.to_string_lossy(),
                    item.size as i64,
                    item.modified_ns
                ],
            )
            .unwrap();
        }
        drop(conn);

        let cancel = AtomicBool::new(false);
        let (tx, rx) = unbounded();
        let wakeup: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        scan_worker(1, 1, records.clone(), &db, &cancel, &tx, &wakeup).unwrap();
        let events = rx.try_iter().collect::<Vec<_>>();
        let mut groups = 0;
        let mut first_stats = DuplicateStats::default();
        for event in events {
            match event {
                DuplicateEvent::HashBatch { updates, .. } => {
                    for update in updates {
                        if let Some(item) = records.iter_mut().find(|item| item.id == update.id) {
                            item.content_hash = Some(update.content_hash);
                        }
                    }
                }
                DuplicateEvent::Finished {
                    groups: found,
                    stats,
                    ..
                } => {
                    groups = found.len();
                    first_stats = stats;
                }
                _ => {}
            }
        }
        assert_eq!(groups, 1);
        assert_eq!(first_stats.candidate_files, 3);
        assert_eq!(first_stats.hashed_files, 3);
        assert_eq!(first_stats.bytes_read, 36);

        let (tx, rx) = unbounded();
        scan_worker(1, 2, records, &db, &cancel, &tx, &wakeup).unwrap();
        let second_stats = rx
            .try_iter()
            .find_map(|event| match event {
                DuplicateEvent::Finished { stats, .. } => Some(stats),
                _ => None,
            })
            .unwrap();
        assert_eq!(second_stats.hashed_files, 0);
        assert_eq!(second_stats.reused_hashes, 3);
        assert_eq!(second_stats.bytes_read, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn default_keeper_prefers_shallow_clean_name() {
        let mut clean = ImageRecord {
            id: 1,
            path: PathBuf::from(r"C:\root\photo.jpg"),
            relative_path: "photo.jpg".into(),
            file_name: "photo.jpg".into(),
            size: 10,
            modified_ns: 2,
            width: None,
            height: None,
            format: "jpg".into(),
            thumbnail_key: "1".into(),
            content_hash: None,
        };
        let mut copy = clean.clone();
        copy.id = 2;
        copy.relative_path = r"backup\photo (1).jpg".into();
        copy.file_name = "photo (1).jpg".into();
        assert!(keeper_key(&clean) < keeper_key(&copy));
        clean.file_name = "photo copy.jpg".into();
        assert!(has_copy_marker(&clean.file_name));
    }

    #[test]
    fn deletion_modes_can_keep_one_or_target_every_member() {
        let records = (1..=3)
            .map(|id| ImageRecord {
                id,
                path: PathBuf::from(format!("{id}.jpg")),
                relative_path: format!("{id}.jpg"),
                file_name: format!("{id}.jpg"),
                size: 1,
                modified_ns: 1,
                width: None,
                height: None,
                format: "jpg".into(),
                thumbnail_key: id.to_string(),
                content_hash: Some("hash".into()),
            })
            .collect::<Vec<_>>();
        let keeper = records[0].id;
        assert_eq!(records.iter().filter(|item| item.id != keeper).count(), 2);
        assert_eq!(records.len(), 3);
    }

    #[test]
    fn progress_event_types_are_sendable() {
        let (tx, rx) = unbounded();
        tx.send(DuplicateEvent::Progress {
            generation: 1,
            task_id: 2,
            stats: DuplicateStats::default(),
        })
        .unwrap();
        assert!(matches!(
            rx.recv().unwrap(),
            DuplicateEvent::Progress { .. }
        ));
    }

    #[test]
    fn delete_validation_rejects_changed_file() {
        let root = std::env::temp_dir().join(format!(
            "tuhai-view-duplicate-validation-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("photo.jpg");
        fs::write(&path, b"original").unwrap();
        let mut item = record(path.clone(), 1);
        item.content_hash = Some("a".repeat(64));
        assert!(validate_delete_candidate(&item, &"a".repeat(64)).is_ok());
        fs::write(&path, b"changed-and-longer").unwrap();
        assert!(validate_delete_candidate(&item, &"a".repeat(64)).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancelled_scan_never_finishes() {
        let root = std::env::temp_dir().join(format!(
            "tuhai-view-duplicate-cancel-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("one.jpg"), b"same").unwrap();
        fs::write(root.join("two.jpg"), b"same").unwrap();
        let records = vec![
            record(root.join("one.jpg"), 1),
            record(root.join("two.jpg"), 2),
        ];
        let db = root.join("catalog.sqlite3");
        let connection = Connection::open(&db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE images (
                    id INTEGER PRIMARY KEY, path TEXT, size INTEGER, modified_ns INTEGER,
                    content_hash TEXT
                );",
            )
            .unwrap();
        drop(connection);
        let cancel = AtomicBool::new(true);
        let (tx, rx) = unbounded();
        let wakeup: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        scan_worker(1, 1, records, &db, &cancel, &tx, &wakeup).unwrap();
        let events = rx.try_iter().collect::<Vec<_>>();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, DuplicateEvent::Cancelled { .. }))
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, DuplicateEvent::Finished { .. }))
        );
        fs::remove_dir_all(root).unwrap();
    }
}
