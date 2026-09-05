use crate::models::ImageRecord;
use anyhow::Result;
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};
#[cfg(test)]
use {
    crate::models::SortMode,
    crossbeam_channel::Sender,
    std::{
        collections::{HashMap, HashSet},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Instant,
    },
    walkdir::WalkDir,
};

const SCHEMA_VERSION: i64 = 3;
// 以事务批量写入，避免上万张图片逐条提交导致 SQLite fsync 成为瓶颈。
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
    pub thumbnail_key: String,
    pub width: u32,
    pub height: u32,
}

#[path = "catalog_runtime.rs"]
mod runtime;
pub use runtime::{CatalogEvent, CatalogService};

#[cfg(test)]
#[derive(Clone, Debug)]
#[allow(dead_code)]
enum ScanEvent {
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

fn init_db(db_path: &Path) -> Result<()> {
    let conn = open_connection(db_path)?;
    let has_table: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='images')",
        [],
        |row| row.get(0),
    )?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if !has_table {
        create_schema(&conn)?;
    } else if version < SCHEMA_VERSION {
        migrate_schema(&conn, version)?;
    }
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

fn open_connection(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    // WAL 让扫描写入与界面读取可以并行；NORMAL 在索引这种可重建数据上换取更高吞吐。
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
            content_hash TEXT,
            UNIQUE(root, path)
        );
        CREATE INDEX IF NOT EXISTS idx_images_root ON images(root);
        CREATE INDEX IF NOT EXISTS idx_images_root_modified ON images(root, modified_ns DESC);
        CREATE INDEX IF NOT EXISTS idx_images_root_hash ON images(root, size, content_hash);",
    )?;
    Ok(())
}

fn migrate_schema(conn: &Connection, version: i64) -> Result<()> {
    if version >= 2 {
        conn.execute_batch(
            "ALTER TABLE images ADD COLUMN content_hash TEXT;
             CREATE INDEX IF NOT EXISTS idx_images_root_hash
                 ON images(root, size, content_hash);",
        )?;
        return Ok(());
    }
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
            content_hash TEXT,
            UNIQUE(root, path)
        );
        INSERT OR REPLACE INTO images_v2
            (id, root, path, relative_path, file_name, size, modified_ns, width, height, format, thumbnail_key, content_hash)
            SELECT id, root, path, relative_path, file_name, size, modified_ns, width, height, format, thumbnail_key, NULL
            FROM images;
        DROP TABLE images;
        ALTER TABLE images_v2 RENAME TO images;
        CREATE INDEX idx_images_root ON images(root);
        CREATE INDEX idx_images_root_modified ON images(root, modified_ns DESC);
        CREATE INDEX idx_images_root_hash ON images(root, size, content_hash);",
    )?;
    tx.commit()?;
    Ok(())
}

fn load_cached_connection(conn: &Connection, root: &Path) -> Result<Vec<ImageRecord>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id,path,relative_path,file_name,size,modified_ns,width,height,format,thumbnail_key,content_hash
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
            content_hash: row.get(10)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
fn scan_worker(
    root: &Path,
    db_path: &Path,
    generation: u64,
    tx: &Sender<ScanEvent>,
    cancel: &AtomicBool,
    sort: SortMode,
    wakeup: Arc<dyn Fn() + Send + Sync>,
) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        return Ok(());
    }
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
    let _ = tx.send(ScanEvent::Snapshot {
        generation,
        records: sorted_snapshot,
    });
    wakeup();

    // 旧快照已经先送往 UI；现在才开始后台磁盘校验，打开大目录无需等待全量遍历。
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
            // 大小、修改时间和格式均未变时复用索引，不读取原图，也不重写数据库。
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
            content_hash: None,
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

    // 只有完整扫描没有遍历错误或取消时，才可将“未见到”的旧记录认定为已删除。
    // 否则保留旧记录，防止无权限目录或临时 I/O 错误误删索引。
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
        let _ = tx.send(ScanEvent::RemoveBatch {
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
    let _ = tx.send(ScanEvent::Finished {
        generation,
        total,
        stats,
    });
    wakeup();
    Ok(())
}

fn upsert_records(conn: &mut Connection, root: &Path, pending: &mut [ImageRecord]) -> Result<()> {
    let transaction = conn.transaction()?;
    {
        let mut statement = transaction.prepare_cached(
            "INSERT INTO images
                (root,path,relative_path,file_name,size,modified_ns,width,height,format,thumbnail_key,content_hash)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
             ON CONFLICT(root,path) DO UPDATE SET
                relative_path=excluded.relative_path,
                file_name=excluded.file_name,
                size=excluded.size,
                modified_ns=excluded.modified_ns,
                width=excluded.width,
                height=excluded.height,
                format=excluded.format,
                thumbnail_key=excluded.thumbnail_key,
                content_hash=excluded.content_hash
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
                    record.content_hash,
                ],
                |row| row.get(0),
            )?;
        }
    }
    transaction.commit()?;
    Ok(())
}

#[cfg(test)]
fn upsert_batch(
    conn: &mut Connection,
    root: &Path,
    pending: &mut Vec<ImageRecord>,
    generation: u64,
    tx: &Sender<ScanEvent>,
    wakeup: &Arc<dyn Fn() + Send + Sync>,
) -> Result<Duration> {
    let started = Instant::now();
    upsert_records(conn, root, pending)?;
    let batch = std::mem::take(pending);
    let _ = tx.send(ScanEvent::UpsertBatch {
        generation,
        records: batch,
    });
    wakeup();
    Ok(started.elapsed())
}

#[cfg(test)]
fn send_progress(tx: &Sender<ScanEvent>, generation: u64, stats: &ScanStats) {
    let _ = tx.send(ScanEvent::Progress {
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

#[cfg(test)]
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
            content_hash: None,
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
                .all(|event| !matches!(event, ScanEvent::Finished { .. }))
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
        migrate_schema(&connection, 1).unwrap();
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
    fn v2_schema_migrates_and_preserves_records() {
        let db = std::env::temp_dir().join(format!("tuhai-view-migrate-v2-{}", std::process::id()));
        let _ = fs::remove_file(&db);
        let connection = Connection::open(&db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE images (
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
                INSERT INTO images(root,path,relative_path,file_name,size,modified_ns,format,thumbnail_key)
                    VALUES ('C:/one','C:/one/a.jpg','a.jpg','a.jpg',1,1,'jpg','one');
                PRAGMA user_version=2;",
            )
            .unwrap();
        migrate_schema(&connection, 2).unwrap();
        let (count, hash): (i64, Option<String>) = connection
            .query_row("SELECT COUNT(*), content_hash FROM images", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(hash, None);
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
                .any(|event| matches!(event, ScanEvent::UpsertBatch { .. }))
        );
        assert!(first.iter().any(
            |event| matches!(event, ScanEvent::Finished { stats, .. } if stats.inserted == 2)
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
                .any(|event| matches!(event, ScanEvent::UpsertBatch { .. }))
        );
        assert!(
            second.iter().any(
                |event| matches!(event, ScanEvent::Finished { stats, .. } if stats.reused == 2)
            )
        );

        fs::remove_file(root.join("b.png")).unwrap();
        let (tx, rx) = unbounded();
        scan_worker(&root, &db, 3, &tx, &cancel, SortMode::ModifiedDesc, wakeup).unwrap();
        let third = rx.try_iter().collect::<Vec<_>>();
        assert!(
            third
                .iter()
                .any(|event| matches!(event, ScanEvent::RemoveBatch { ids, .. } if ids.len() == 1))
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
                content_hash: None,
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
