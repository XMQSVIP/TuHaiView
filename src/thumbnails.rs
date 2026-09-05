use crate::models::ImageRecord;
use crate::storage;
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, bounded};
use image::{ImageDecoder, ImageReader, imageops::FilterType};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
};

const CACHE_START_LIMIT: u64 = 1024 * 1024 * 1024;
const CACHE_TARGET_LIMIT: u64 = 800 * 1024 * 1024;
const CACHE_MAGIC: &[u8; 8] = b"RIPTHM2\0";
const CACHE_HEADER_LEN: usize = 24;
static CACHE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageKind {
    Thumbnail,
    Preview,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThumbnailPriority {
    // 沉浸预览必须抢占缩略图解码，保证切图时的响应感。
    Preview,
    // 视口内卡片。
    Visible,
    // 视口上下各一屏的预取，可随滚动丢弃。
    Prefetch,
}

#[derive(Clone)]
struct Request {
    generation: u64,
    prefetch_epoch: u64,
    cache_epoch: u64,
    record: ImageRecord,
    max_side: u32,
    kind: ImageKind,
    priority: ThumbnailPriority,
}

struct DecodedImage {
    pixels: Vec<u8>,
    width: usize,
    height: usize,
    source_width: u32,
    source_height: u32,
}

pub struct ImageResult {
    pub generation: u64,
    pub record_id: i64,
    pub path: PathBuf,
    pub modified_ns: i64,
    pub texture_key: String,
    pub pixels: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub source_width: u32,
    pub source_height: u32,
    pub error: Option<String>,
}

struct CacheState {
    directory: PathBuf,
    bytes: AtomicU64,
    cleanup_running: AtomicBool,
    io_gate: parking_lot::RwLock<()>,
}

impl CacheState {
    fn new() -> Result<Arc<Self>> {
        let directory = storage::thumbnail_cache_dir()?;
        let mut bytes = 0_u64;
        for entry in fs::read_dir(&directory)?.flatten() {
            let path = entry.path();
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(".tmp-"))
            {
                let _ = fs::remove_file(path);
                continue;
            }
            if let Ok(metadata) = entry.metadata()
                && metadata.is_file()
            {
                bytes = bytes.saturating_add(metadata.len());
            }
        }
        let state = Arc::new(Self {
            directory,
            bytes: AtomicU64::new(bytes),
            cleanup_running: AtomicBool::new(false),
            io_gate: parking_lot::RwLock::new(()),
        });
        state.maybe_cleanup();
        Ok(state)
    }

    fn path_for(&self, thumbnail_key: &str) -> PathBuf {
        self.directory.join(format!("{thumbnail_key}.rgba"))
    }

    fn record_write(self: &Arc<Self>, old_size: u64, new_size: u64) {
        if new_size >= old_size {
            self.bytes.fetch_add(new_size - old_size, Ordering::Relaxed);
        } else {
            self.bytes.fetch_sub(old_size - new_size, Ordering::Relaxed);
        }
        self.maybe_cleanup();
    }

    fn maybe_cleanup(self: &Arc<Self>) {
        if self.bytes.load(Ordering::Relaxed) <= CACHE_START_LIMIT
            || self
                .cleanup_running
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        // 清理在后台执行，并由原子标记保证任意时刻至多一个清理任务，
        // 写入路径只维护字节计数，不会反复遍历整个缓存目录。
        let state = self.clone();
        let _ = thread::Builder::new()
            .name("thumbnail-cache-cleaner".into())
            .spawn(move || {
                let _ = state.cleanup();
                state.cleanup_running.store(false, Ordering::Release);
            });
    }

    fn cleanup(&self) -> Result<()> {
        let _io_guard = self.io_gate.write();
        let mut files = Vec::new();
        let mut total = 0_u64;
        for entry in fs::read_dir(&self.directory)?.flatten() {
            let path = entry.path();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            total = total.saturating_add(metadata.len());
            files.push((metadata.modified().ok(), metadata.len(), path));
        }
        files.sort_by_key(|(modified, _, _)| *modified);
        for (_, size, path) in files {
            if total <= CACHE_TARGET_LIMIT {
                break;
            }
            if fs::remove_file(path).is_ok() {
                total = total.saturating_sub(size);
            }
        }
        self.bytes.store(total, Ordering::Relaxed);
        Ok(())
    }

    fn clear(&self) -> Result<()> {
        let _io_guard = self.io_gate.write();
        storage::clear_thumbnail_cache()?;
        self.bytes.store(0, Ordering::Relaxed);
        Ok(())
    }
}

pub struct ThumbnailService {
    preview_tx: Sender<Request>,
    visible_tx: Sender<Request>,
    prefetch_tx: Sender<Request>,
    pub rx: Receiver<ImageResult>,
    pending: Arc<parking_lot::Mutex<HashSet<(u64, u64, String, ThumbnailPriority)>>>,
    generation: Arc<AtomicU64>,
    prefetch_epoch: Arc<AtomicU64>,
    cache_epoch: Arc<AtomicU64>,
    cache: Option<Arc<CacheState>>,
}

impl ThumbnailService {
    pub fn new(wakeup: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (preview_tx, preview_rx) = bounded::<Request>(8);
        let (visible_tx, visible_rx) = bounded::<Request>(128);
        let (prefetch_tx, prefetch_rx) = bounded::<Request>(128);
        let (result_tx, rx) = bounded::<ImageResult>(64);
        let pending = Arc::new(parking_lot::Mutex::new(HashSet::new()));
        let generation = Arc::new(AtomicU64::new(0));
        let prefetch_epoch = Arc::new(AtomicU64::new(0));
        let cache_epoch = Arc::new(AtomicU64::new(0));
        let cache = CacheState::new().ok();
        // 留出一个核心给 UI 和系统；限制上限避免高核机器同时解码过多大图。
        let workers = std::thread::available_parallelism()
            .map(|count| count.get().saturating_sub(1))
            .unwrap_or(2)
            .clamp(2, 8);

        for index in 0..workers {
            let preview_rx = preview_rx.clone();
            let visible_rx = visible_rx.clone();
            let prefetch_rx = prefetch_rx.clone();
            let result_tx = result_tx.clone();
            let pending = pending.clone();
            let generation_state = generation.clone();
            let prefetch_epoch_state = prefetch_epoch.clone();
            let cache_epoch_state = cache_epoch.clone();
            let cache = cache.clone();
            let wakeup = wakeup.clone();
            thread::Builder::new()
                .name(format!("image-decoder-{index}"))
                .spawn(move || loop {
                    // 固定优先级：预览 > 当前可见项 > 预取。
                    let request = crossbeam_channel::select_biased! {
                        recv(preview_rx) -> request => match request { Ok(request) => request, Err(_) => break },
                        recv(visible_rx) -> request => match request { Ok(request) => request, Err(_) => break },
                        recv(prefetch_rx) -> request => match request { Ok(request) => request, Err(_) => break },
                    };
                    let key = texture_key(&request.record, request.kind);
                    let pending_key = (
                        request.generation,
                        request.prefetch_epoch,
                        key.clone(),
                        request.priority,
                    );
                    let stale_generation =
                        request.generation != generation_state.load(Ordering::Acquire);
                    let stale_prefetch = request.priority == ThumbnailPriority::Prefetch
                        && request.prefetch_epoch
                            != prefetch_epoch_state.load(Ordering::Acquire);
                    // 目录切换或快速滚动后，旧任务不解码、不回传，避免占用 CPU 和显存。
                    if stale_generation || stale_prefetch {
                        pending.lock().remove(&pending_key);
                        continue;
                    }
                    let loaded = load_image(
                        &request.record,
                        request.max_side,
                        request.kind,
                        cache.as_ref(),
                        request.generation,
                        &generation_state,
                        request.cache_epoch,
                        &cache_epoch_state,
                    );
                    let stale_generation =
                        request.generation != generation_state.load(Ordering::Acquire);
                    let stale_prefetch = request.priority == ThumbnailPriority::Prefetch
                        && request.prefetch_epoch
                            != prefetch_epoch_state.load(Ordering::Acquire);
                    if stale_generation || stale_prefetch {
                        pending.lock().remove(&pending_key);
                        continue;
                    }
                    let result = match loaded {
                        Ok(image) => ImageResult {
                            generation: request.generation,
                            record_id: request.record.id,
                            path: request.record.path.clone(),
                            modified_ns: request.record.modified_ns,
                            texture_key: key.clone(),
                            pixels: image.pixels,
                            width: image.width,
                            height: image.height,
                            source_width: image.source_width,
                            source_height: image.source_height,
                            error: None,
                        },
                        Err(error) => ImageResult {
                            generation: request.generation,
                            record_id: request.record.id,
                            path: request.record.path.clone(),
                            modified_ns: request.record.modified_ns,
                            texture_key: key.clone(),
                            pixels: Vec::new(),
                            width: 0,
                            height: 0,
                            source_width: 0,
                            source_height: 0,
                            error: Some(error.to_string()),
                        },
                    };
                    pending.lock().remove(&pending_key);
                    if result_tx.try_send(result).is_ok() {
                        wakeup();
                    }
                })
                .expect("failed to create image decoder thread");
        }

        Self {
            preview_tx,
            visible_tx,
            prefetch_tx,
            rx,
            pending,
            generation,
            prefetch_epoch,
            cache_epoch,
            cache,
        }
    }

    pub fn set_generation(&mut self, generation: u64) {
        // generation 是根目录会话号；清空 pending 仅为去重集合，工作线程仍会自行检查旧请求。
        self.generation.store(generation, Ordering::Release);
        self.prefetch_epoch.fetch_add(1, Ordering::AcqRel);
        self.pending.lock().clear();
    }

    pub fn advance_prefetch_epoch(&self) {
        // 仅使旧预取失效，不影响已在视口中的缩略图任务。
        self.prefetch_epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub fn request_thumbnail(&self, record: ImageRecord, priority: ThumbnailPriority) {
        debug_assert!(priority != ThumbnailPriority::Preview);
        self.request(record, 256, ImageKind::Thumbnail, priority);
    }

    pub fn request_preview(&self, record: ImageRecord, max_side: u32) {
        self.request(
            record,
            max_side,
            ImageKind::Preview,
            ThumbnailPriority::Preview,
        );
    }

    pub fn clear_disk_cache(&self) -> Result<()> {
        // 先递增 epoch，防止正在解码的旧任务在清理完成后又把缓存写回来。
        self.cache_epoch.fetch_add(1, Ordering::AcqRel);
        if let Some(cache) = &self.cache {
            cache.clear()
        } else {
            storage::clear_thumbnail_cache()
        }
    }

    fn request(
        &self,
        record: ImageRecord,
        max_side: u32,
        kind: ImageKind,
        priority: ThumbnailPriority,
    ) {
        let generation = self.generation.load(Ordering::Acquire);
        let prefetch_epoch = self.prefetch_epoch.load(Ordering::Acquire);
        let cache_epoch = self.cache_epoch.load(Ordering::Acquire);
        let key = texture_key(&record, kind);
        let pending_epoch = if priority == ThumbnailPriority::Prefetch {
            prefetch_epoch
        } else {
            0
        };
        let pending_key = (generation, pending_epoch, key, priority);
        let mut pending = self.pending.lock();
        if !pending.insert(pending_key.clone()) {
            return;
        }
        let sender = match priority {
            ThumbnailPriority::Preview => &self.preview_tx,
            ThumbnailPriority::Visible => &self.visible_tx,
            ThumbnailPriority::Prefetch => &self.prefetch_tx,
        };
        if sender
            .try_send(Request {
                generation,
                prefetch_epoch: pending_epoch,
                cache_epoch,
                record,
                max_side,
                kind,
                priority,
            })
            .is_err()
        {
            pending.remove(&pending_key);
        }
    }
}

pub fn texture_key(record: &ImageRecord, kind: ImageKind) -> String {
    let suffix = match kind {
        ImageKind::Thumbnail => "thumb",
        ImageKind::Preview => "preview",
    };
    format!("{}:{suffix}", record.thumbnail_key)
}

fn load_image(
    record: &ImageRecord,
    max_side: u32,
    kind: ImageKind,
    cache: Option<&Arc<CacheState>>,
    generation: u64,
    generation_state: &AtomicU64,
    cache_epoch: u64,
    cache_epoch_state: &AtomicU64,
) -> Result<DecodedImage> {
    if kind == ImageKind::Thumbnail {
        if let Some(cache) = cache {
            let cached_path = cache.path_for(&record.thumbnail_key);
            if let Some(image) = read_cache(&cached_path) {
                return Ok(image);
            }
            let image = decode_scaled(record, max_side)?;
            if generation == generation_state.load(Ordering::Acquire) {
                write_cache(
                    &cached_path,
                    &image,
                    cache,
                    Some((generation_state, generation)),
                    Some((cache_epoch_state, cache_epoch)),
                )?;
            }
            return Ok(image);
        }
    }
    decode_scaled(record, max_side)
}

fn decode_scaled(record: &ImageRecord, max_side: u32) -> Result<DecodedImage> {
    let reader = ImageReader::open(&record.path)?.with_guessed_format()?;
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation().ok();
    let mut image = image::DynamicImage::from_decoder(decoder)?;
    if let Some(orientation) = orientation {
        image.apply_orientation(orientation);
    }
    let source_width = image.width();
    let source_height = image.height();
    let resized = image
        .resize(max_side, max_side, FilterType::Triangle)
        .to_rgba8();
    Ok(DecodedImage {
        width: resized.width() as usize,
        height: resized.height() as usize,
        pixels: resized.into_raw(),
        source_width,
        source_height,
    })
}

fn read_cache(path: &Path) -> Option<DecodedImage> {
    let bytes = fs::read(path).ok()?;
    if bytes.len() < CACHE_HEADER_LEN || &bytes[0..8] != CACHE_MAGIC {
        return None;
    }
    let source_width = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let source_height = u32::from_le_bytes(bytes[12..16].try_into().ok()?);
    let width = u32::from_le_bytes(bytes[16..20].try_into().ok()?) as usize;
    let height = u32::from_le_bytes(bytes[20..24].try_into().ok()?) as usize;
    let pixel_length = width.checked_mul(height)?.checked_mul(4)?;
    if source_width == 0
        || source_height == 0
        || width == 0
        || height == 0
        || bytes.len() != CACHE_HEADER_LEN + pixel_length
    {
        return None;
    }
    Some(DecodedImage {
        pixels: bytes[CACHE_HEADER_LEN..].to_vec(),
        width,
        height,
        source_width,
        source_height,
    })
}

fn write_cache(
    path: &Path,
    image: &DecodedImage,
    cache: &Arc<CacheState>,
    generation: Option<(&AtomicU64, u64)>,
    cache_epoch: Option<(&AtomicU64, u64)>,
) -> Result<()> {
    let is_stale = || {
        generation.is_some_and(|(state, expected)| state.load(Ordering::Acquire) != expected)
            || cache_epoch
                .is_some_and(|(state, expected)| state.load(Ordering::Acquire) != expected)
    };
    if is_stale() {
        return Ok(());
    }
    let mut bytes = Vec::with_capacity(CACHE_HEADER_LEN + image.pixels.len());
    bytes.extend_from_slice(CACHE_MAGIC);
    bytes.extend_from_slice(&image.source_width.to_le_bytes());
    bytes.extend_from_slice(&image.source_height.to_le_bytes());
    bytes.extend_from_slice(&(image.width as u32).to_le_bytes());
    bytes.extend_from_slice(&(image.height as u32).to_le_bytes());
    bytes.extend_from_slice(&image.pixels);
    // 清缓存时持有写锁；普通写入持读锁，使“清空”和“临时文件原子替换”不会交错。
    let _io_guard = cache.io_gate.read();
    if is_stale() {
        return Ok(());
    }
    let old_size = fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let counter = CACHE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("thumbnail");
    let temporary =
        path.with_file_name(format!("{file_name}.tmp-{}-{counter}", std::process::id()));
    fs::write(&temporary, &bytes)?;
    if is_stale() {
        let _ = fs::remove_file(temporary);
        return Ok(());
    }
    // Writes only follow cache misses. Replace a malformed existing entry,
    // while normal first writes still install the completed temporary file
    // with one rename.
    if old_size > 0 {
        fs::remove_file(path)?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(temporary);
        return Err(error.into());
    }
    cache.record_write(old_size, bytes.len() as u64);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_v2_header_round_trips_dimensions() {
        let root = std::env::temp_dir().join(format!("tuhai-view-thumb-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("thumb.rgba");
        let cache = Arc::new(CacheState {
            directory: root.clone(),
            bytes: AtomicU64::new(0),
            cleanup_running: AtomicBool::new(false),
            io_gate: parking_lot::RwLock::new(()),
        });
        let image = DecodedImage {
            pixels: vec![1, 2, 3, 4, 5, 6, 7, 8],
            width: 2,
            height: 1,
            source_width: 4000,
            source_height: 3000,
        };
        write_cache(&path, &image, &cache, None, None).unwrap();
        let loaded = read_cache(&path).unwrap();
        assert_eq!((loaded.source_width, loaded.source_height), (4000, 3000));
        assert_eq!((loaded.width, loaded.height), (2, 1));
        assert_eq!(loaded.pixels, image.pixels);
        fs::write(&path, b"broken-cache").unwrap();
        write_cache(&path, &image, &cache, None, None).unwrap();
        assert_eq!(read_cache(&path).unwrap().pixels, image.pixels);
        fs::remove_dir_all(root).unwrap();
    }
}
