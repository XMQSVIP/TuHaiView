//! Best-effort compressed cache. One maintenance thread owns the manifest and writes.
use crate::{
    budget::{ByteBudget, ByteLease},
    decoding::DecodedImage,
    performance::{self, MIB, PerformanceSettings},
    storage,
};
use anyhow::{Result, bail};
use crossbeam_channel::{Sender, bounded};
use image::ImageEncoder;
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAGIC: &[u8; 8] = b"RIPTHM3\0";
const OLD_MAGIC: &[u8; 8] = b"RIPTHM2\0";
const HEADER: usize = 32;
const MAX_FILE: u64 = 2 * MIB as u64;
struct Write {
    key: String,
    image: Arc<DecodedImage>,
    jpeg: bool,
    epoch: u64,
    _lease: ByteLease,
}
#[derive(Default)]
struct Control {
    clear: bool,
    settings: Option<PerformanceSettings>,
    touches: HashMap<String, u64>,
}
pub struct DiskCache {
    directory: Arc<Mutex<Option<PathBuf>>>,
    tx: Sender<Write>,
    control: Arc<Mutex<Control>>,
    epoch: Arc<AtomicU64>,
    budget: Arc<ByteBudget>,
    closed: Arc<AtomicBool>,
    pub settings: Arc<Mutex<PerformanceSettings>>,
    pub status: Arc<Mutex<Option<String>>>,
}
impl DiskCache {
    pub fn new(wakeup: Arc<dyn Fn() + Send + Sync>) -> Arc<Self> {
        let (tx, rx) = bounded::<Write>(128);
        let cache = Arc::new(Self {
            directory: Arc::default(),
            tx,
            control: Arc::default(),
            epoch: Arc::new(AtomicU64::new(0)),
            budget: ByteBudget::new(performance::CACHE_QUEUE_BYTES, 0),
            closed: Arc::new(AtomicBool::new(false)),
            settings: Arc::new(Mutex::new(PerformanceSettings::default())),
            status: Arc::default(),
        });
        let directory = cache.directory.clone();
        let control = cache.control.clone();
        let epoch = cache.epoch.clone();
        let closed = cache.closed.clone();
        let settings = cache.settings.clone();
        let status = cache.status.clone();
        thread::Builder::new().name("thumbnail-cache-owner".into()).spawn(move || {
            let setup = (|| -> Result<_> {
                let dir = storage::thumbnail_cache_dir()?;
                let settings_path = storage::data_dir()?.join("performance-settings.json");
                let mut config: PerformanceSettings = fs::read(&settings_path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default(); config.validate();
                *settings.lock() = config;
                let conn = manifest(&dir)?; Ok((dir, settings_path, conn))
            })();
            let (dir, settings_path, mut conn) = match setup { Ok(value)=>value, Err(error)=> { *status.lock()=Some(format!("缩略图缓存不可用：{error}")); wakeup(); return; } };
            *directory.lock() = Some(dir.clone()); wakeup();
            // Reconciliation is incremental; cache reads can proceed immediately.
            let mut reconciliation = Some(walkdir::WalkDir::new(&dir).into_iter());
            let mut last_maintenance = Instant::now();
            let mut cleaning = false;
            while !closed.load(Ordering::Acquire) {
                let (clear, config) = { let mut c=control.lock(); (std::mem::take(&mut c.clear),c.settings.take()) };
                if clear {
                    reconciliation=None;
                    let result=(|| -> Result<()> {
                        // The owner serializes writes and clear; epoch invalidates queued pre-clear work.
                        conn.execute("DELETE FROM entries",[])?;
                        for entry in walkdir::WalkDir::new(&dir).min_depth(1).contents_first(true) {
                            let entry=entry?; let p=entry.path();
                            if entry.file_type().is_file() && is_payload(p) { fs::remove_file(p)?; }
                            else if entry.file_type().is_dir() { let _=fs::remove_dir(p); }
                        }
                        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?; Ok(())
                    })();
                    *status.lock()=Some(match result { Ok(())=>"缩略图缓存已清理".into(),Err(error)=>format!("清理缩略图缓存失败：{error}") }); wakeup();
                }
                if let Some(mut config)=config {
                    config.validate(); *settings.lock()=config.clone();
                    let result=serde_json::to_vec_pretty(&config).map_err(anyhow::Error::from).and_then(|b| { let tmp=settings_path.with_extension("json.tmp"); fs::write(&tmp,b)?; replace(&tmp,&settings_path)?; Ok(()) });
                    if let Err(error)=result { *status.lock()=Some(format!("设置保存失败：{error}")); wakeup(); }
                    last_maintenance=Instant::now()-Duration::from_secs(2);
                }
                // Yield between work units, retaining clear/settings responsiveness.
                if performance::PREVIEW_BUSY.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                match rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(write) if write.epoch==epoch.load(Ordering::Acquire) => {
                        let result=write_entry(&dir,&mut conn,&write,&epoch);
                        if let Err(error)=result { performance::sample("cache_write_error",1.0); tracing::warn!(%error,"cache write failed"); }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected)=>break, _=>{}
                }
                if let Some(iter)=reconciliation.as_mut() {
                    let start=Instant::now(); let mut done=false;
                    for _ in 0..64 {
                        let Some(entry)=iter.next() else { done=true;break; };
                        if let Ok(entry)=entry { if entry.file_type().is_file() && is_payload(entry.path()) {
                            let rel=entry.path().strip_prefix(&dir).unwrap().to_string_lossy().into_owned();
                            if rel.contains(".tmp-") { let _=fs::remove_file(entry.path()); }
                            else if let Ok(meta)=entry.metadata() { let _=conn.execute("INSERT INTO entries(path,bytes,accessed) VALUES(?1,?2,?3) ON CONFLICT(path) DO UPDATE SET bytes=excluded.bytes",params![rel,meta.len() as i64,now() as i64]); }
                        } }
                        if start.elapsed()>=Duration::from_millis(2) { break; }
                    }
                    if done { reconciliation=None; }
                }
                if last_maintenance.elapsed()>=Duration::from_secs(1) || cleaning {
                    let touches=std::mem::take(&mut control.lock().touches);
                    if let Ok(tx)=conn.transaction() { for (path,time) in touches { let _=tx.execute("UPDATE entries SET accessed=?1 WHERE path=?2",params![time as i64,path]); } let _=tx.commit(); }
                    let limit=settings.lock().disk_cache_gib*1024*1024*1024;
                    match cleanup(&dir,&conn,limit,cleaning) {
                        Ok(pending) => cleaning=pending,
                        Err(error) => { cleaning=false; tracing::warn!(%error,"cache maintenance failed"); }
                    }
                    last_maintenance=Instant::now();
                }
            }
        }).expect("cache owner thread");
        cache
    }
    pub fn record_metrics(&self) {
        if !performance::enabled() {
            return;
        }
        performance::gauge("cache_queue_bytes", self.budget.used() as f64);
        performance::gauge("cache_queue_count", self.tx.len() as f64);
    }
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }
    pub fn read(&self, key: &str) -> Option<(DecodedImage, bool)> {
        if !valid_key(key) {
            return None;
        }
        let dir = self.directory.lock().clone()?;
        let rel = relative_path(key);
        let path = dir.join(&rel);
        let (result, legacy) = if let Some(image) = read_file(&path) {
            (image, false)
        } else {
            (read_file(&dir.join(format!("{key}.rgba")))?, true)
        };
        let touch = if !legacy { rel } else { format!("{key}.rgba") };
        let mut c = self.control.lock();
        if c.touches.len() < 4096 {
            c.touches.insert(touch, now());
        }
        Some((result, legacy))
    }
    pub fn write(&self, key: String, image: Arc<DecodedImage>, jpeg: bool, epoch: u64) {
        if !valid_key(&key) {
            return;
        }
        let Some(lease) = self.budget.try_acquire(image.pixels.len()) else {
            return;
        };
        let _ = self.tx.try_send(Write {
            key,
            image,
            jpeg,
            epoch,
            _lease: lease,
        });
    }
    pub fn clear(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.control.lock().clear = true;
    }
    pub fn set_limit(&self, gib: u64) {
        let mut config = PerformanceSettings {
            disk_cache_gib: gib,
        };
        config.validate();
        *self.settings.lock() = config.clone();
        self.control.lock().settings = Some(config);
    }
}
impl Drop for DiskCache {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
    }
}
fn relative_path(key: &str) -> String {
    format!("{}/{}-v3-256.thm", key.get(..2).unwrap_or("00"), key)
}
fn valid_key(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit())
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn is_payload(p: &Path) -> bool {
    p.extension().is_some_and(|e| e == "rgba" || e == "thm")
        || p.file_name()
            .is_some_and(|n| n.to_string_lossy().contains(".tmp-"))
}
fn manifest(dir: &Path) -> Result<Connection> {
    let conn = Connection::open(dir.join("manifest.sqlite3"))?;
    conn.busy_timeout(Duration::from_secs(2))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA user_version=1; CREATE TABLE IF NOT EXISTS entries(path TEXT PRIMARY KEY,bytes INTEGER NOT NULL,accessed INTEGER NOT NULL); CREATE INDEX IF NOT EXISTS entries_accessed ON entries(accessed);")?;
    Ok(conn)
}
fn encode(image: &DecodedImage, jpeg: bool) -> Result<Vec<u8>> {
    let jpeg = jpeg && image.pixels.chunks_exact(4).all(|p| p[3] == 255);
    let mut payload = Vec::new();
    if jpeg {
        let rgb: Vec<u8> = image
            .pixels
            .chunks_exact(4)
            .flat_map(|p| p[..3].iter().copied())
            .collect();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut payload, 85).encode(
            &rgb,
            image.width as u32,
            image.height as u32,
            image::ExtendedColorType::Rgb8,
        )?;
    } else {
        image::codecs::webp::WebPEncoder::new_lossless(&mut payload).write_image(
            &image.pixels,
            image.width as u32,
            image.height as u32,
            image::ExtendedColorType::Rgba8,
        )?;
    }
    let mut bytes = Vec::with_capacity(HEADER + payload.len());
    bytes.extend_from_slice(MAGIC);
    for value in [
        image.source_width,
        image.source_height,
        image.width as u32,
        image.height as u32,
        if jpeg { 1 } else { 2 },
        payload.len() as u32,
    ] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend(payload);
    Ok(bytes)
}
fn read_file(path: &Path) -> Option<DecodedImage> {
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if !(24..=MAX_FILE).contains(&len) {
        return None;
    }
    let mut bytes = Vec::with_capacity(len as usize);
    file.by_ref()
        .take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 != len {
        return None;
    }
    decode_bytes(&bytes)
}
fn decode_bytes(bytes: &[u8]) -> Option<DecodedImage> {
    if bytes.len() < 24 {
        return None;
    }
    let value = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
    let (source_width, source_height, width, height) =
        (value(8), value(12), value(16) as usize, value(20) as usize);
    if source_width == 0
        || source_height == 0
        || width == 0
        || height == 0
        || width > 256
        || height > 256
    {
        return None;
    }
    let pixels = if &bytes[..8] == OLD_MAGIC {
        if bytes.len() != 24 + width * height * 4 {
            return None;
        }
        bytes[24..].to_vec()
    } else if &bytes[..8] == MAGIC {
        if bytes.len() < HEADER || bytes.len() != HEADER + value(28) as usize {
            return None;
        }
        let format = match value(24) {
            1 => image::ImageFormat::Jpeg,
            2 => image::ImageFormat::WebP,
            _ => return None,
        };
        let mut reader =
            image::ImageReader::with_format(std::io::Cursor::new(&bytes[HEADER..]), format);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(256);
        limits.max_image_height = Some(256);
        limits.max_alloc = Some(2 * MIB as u64);
        reader.limits(limits);
        let decoded = reader.decode().ok()?.into_rgba8();
        if decoded.dimensions() != (width as u32, height as u32) {
            return None;
        }
        decoded.into_raw()
    } else {
        return None;
    };
    Some(DecodedImage {
        pixels,
        width,
        height,
        source_width,
        source_height,
    })
}
fn write_entry(dir: &Path, conn: &mut Connection, write: &Write, epoch: &AtomicU64) -> Result<()> {
    let started = Instant::now();
    let bytes = encode(&write.image, write.jpeg)?;
    if epoch.load(Ordering::Acquire) != write.epoch {
        return Ok(());
    }
    let rel = relative_path(&write.key);
    let path = dir.join(&rel);
    fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension(format!("thm.tmp-{}", std::process::id()));
    fs::write(&tmp, &bytes)?;
    if epoch.load(Ordering::Acquire) != write.epoch {
        let _ = fs::remove_file(tmp);
        return Ok(());
    }
    if let Err(e) = replace(&tmp, &path) {
        let _ = fs::remove_file(tmp);
        return Err(e);
    }
    conn.execute("INSERT INTO entries(path,bytes,accessed) VALUES(?1,?2,?3) ON CONFLICT(path) DO UPDATE SET bytes=excluded.bytes,accessed=excluded.accessed",params![rel,bytes.len() as i64,now() as i64])?;
    let legacy = dir.join(format!("{}.rgba", write.key));
    if fs::remove_file(legacy).is_ok() {
        conn.execute(
            "DELETE FROM entries WHERE path=?1",
            params![format!("{}.rgba", write.key)],
        )?;
    }
    performance::sample("cache_encoded_bytes", bytes.len() as f64);
    performance::elapsed("cache_write_ms", started);
    Ok(())
}
fn replace(source: &Path, target: &Path) -> Result<()> {
    #[cfg(windows)]
    unsafe {
        use windows::{
            Win32::Storage::FileSystem::{
                MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
            },
            core::HSTRING,
        };
        MoveFileExW(
            &HSTRING::from(source),
            &HSTRING::from(target),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )?;
    }
    #[cfg(not(windows))]
    fs::rename(source, target)?;
    Ok(())
}
fn cleanup(dir: &Path, conn: &Connection, limit: u64, cleaning: bool) -> Result<bool> {
    let mut total = conn.query_row("SELECT COALESCE(SUM(bytes),0) FROM entries", [], |r| {
        r.get::<_, u64>(0)
    })?;
    let overhead = [
        "manifest.sqlite3",
        "manifest.sqlite3-wal",
        "manifest.sqlite3-shm",
    ]
    .iter()
    .filter_map(|p| fs::metadata(dir.join(p)).ok())
    .map(|m| m.len())
    .sum::<u64>();
    total = total.saturating_add(overhead);
    if total <= if cleaning { limit * 8 / 10 } else { limit } {
        return Ok(false);
    }
    let mut stmt = conn.prepare("SELECT path,bytes FROM entries ORDER BY accessed LIMIT 128")?;
    let victims = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut removed = false;
    for (rel, size) in victims {
        if total <= limit * 8 / 10 {
            break;
        }
        let path = dir.join(&rel);
        if !path.starts_with(dir)
            || Path::new(&rel).components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            bail!("无效缓存清单路径");
        }
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => continue,
        }
        conn.execute("DELETE FROM entries WHERE path=?1", [rel])?;
        removed = true;
        total = total.saturating_sub(size);
    }
    Ok(removed && total > limit * 8 / 10)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleanup_finishes_to_eighty_percent_across_batches() {
        let dir =
            std::env::temp_dir().join(format!("tuhai-cache-test-{}-{}", std::process::id(), now()));
        fs::create_dir_all(&dir).unwrap();
        let conn = manifest(&dir).unwrap();
        // 400 entries exceeds a single maintenance slice. Large accounted sizes
        // exercise eviction policy without allocating a large test fixture.
        for i in 0..400 {
            let name = format!("{i}.rgba");
            fs::write(dir.join(&name), b"x").unwrap();
            conn.execute(
                "INSERT INTO entries VALUES(?1,1000000,?2)",
                params![name, i],
            )
            .unwrap();
        }
        let mut pending = cleanup(&dir, &conn, 200_000_000, false).unwrap();
        assert!(pending);
        while pending {
            pending = cleanup(&dir, &conn, 200_000_000, true).unwrap();
        }
        let sum: u64 = conn
            .query_row("SELECT SUM(bytes) FROM entries", [], |r| r.get(0))
            .unwrap();
        assert!(sum <= 160_000_000);
        let oldest: u64 = conn
            .query_row("SELECT MIN(accessed) FROM entries", [], |r| r.get(0))
            .unwrap();
        assert!(oldest >= 240);
        assert!(!valid_key("../../outside"));
        drop(conn);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn compressed_cache_roundtrip_alpha_and_source_dimensions() {
        let image = DecodedImage {
            pixels: vec![1, 2, 3, 4, 5, 6, 7, 255],
            width: 2,
            height: 1,
            source_width: 4000,
            source_height: 3000,
        };
        let decoded = decode_bytes(&encode(&image, false).unwrap()).unwrap();
        assert_eq!(decoded.pixels, image.pixels);
        assert_eq!(decoded.source_width, 4000);
        let mut broken = encode(&image, false).unwrap();
        broken[16..20].copy_from_slice(&5000_u32.to_le_bytes());
        assert!(decode_bytes(&broken).is_none());
    }
    #[test]
    fn jpeg_cache_is_small_and_v2_is_readable() {
        let image = DecodedImage {
            pixels: vec![255; 256 * 256 * 4],
            width: 256,
            height: 256,
            source_width: 6000,
            source_height: 4000,
        };
        let encoded = encode(&image, true).unwrap();
        assert!(encoded.len() < image.pixels.len() / 5);
        assert_eq!(
            decode_bytes(&encoded).unwrap().pixels.len(),
            image.pixels.len()
        );
        let mut old = OLD_MAGIC.to_vec();
        for v in [6000u32, 4000, 256, 256] {
            old.extend(v.to_le_bytes());
        }
        old.extend(&image.pixels);
        assert!(decode_bytes(&old).is_some());
    }
    #[test]
    fn stale_writes_and_storage_errors_do_not_publish_entries() {
        let dir = std::env::temp_dir().join(format!(
            "tuhai-stale-cache-{}-{}",
            std::process::id(),
            now()
        ));
        fs::create_dir_all(&dir).unwrap();
        let mut conn = manifest(&dir).unwrap();
        let budget = ByteBudget::new(1024, 0);
        let write = Write {
            key: "a".repeat(64),
            image: Arc::new(DecodedImage {
                pixels: vec![255; 4],
                width: 1,
                height: 1,
                source_width: 1,
                source_height: 1,
            }),
            jpeg: true,
            epoch: 1,
            _lease: budget.try_acquire(4).unwrap(),
        };
        write_entry(&dir, &mut conn, &write, &AtomicU64::new(2)).unwrap();
        assert!(!dir.join(relative_path(&write.key)).exists());
        // A file occupying the shard directory simulates an unwritable cache destination.
        fs::write(dir.join("aa"), b"blocked").unwrap();
        assert!(write_entry(&dir, &mut conn, &write, &AtomicU64::new(1)).is_err());
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        drop(write);
        assert_eq!(budget.used(), 0);
        drop(conn);
        fs::remove_dir_all(dir).unwrap();
    }
}
