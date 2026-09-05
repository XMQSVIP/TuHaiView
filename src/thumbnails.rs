use crate::{
    budget::{ByteBudget, ByteLease},
    decoding::{self},
    models::ImageRecord,
    performance::{self, MIB},
    thumbnail_cache::DiskCache,
};
use crossbeam_channel::{Receiver, Sender, bounded};
use parking_lot::{Condvar, Mutex};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageKind {
    Thumbnail,
    Preview,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ThumbnailPriority {
    Preview,
    Visible,
    Prefetch,
}
#[derive(Clone)]
struct Request {
    generation: u64,
    preview_epoch: u64,
    cache_epoch: u64,
    record: Arc<ImageRecord>,
    max_side: u32,
    kind: ImageKind,
    priority: ThumbnailPriority,
    serial: u64,
}
impl Request {
    fn key(&self) -> String {
        request_key(&self.record, self.kind, self.max_side)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    Decode,
    ResourceLimit,
}
pub struct ImageResult {
    pub generation: u64,
    pub preview_epoch: u64,
    pub record_id: i64,
    pub path: PathBuf,
    pub modified_ns: i64,
    pub source_key: String,
    pub texture_key: String,
    pub request_key: String,
    pub kind: ImageKind,
    pub max_side: u32,
    /// Premultiplied sRGBA, prepared off the UI thread.
    pub pixels: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub source_width: u32,
    pub source_height: u32,
    pub error: Option<String>,
    pub failure: Option<FailureKind>,
    pub _lease: Option<ByteLease>,
}
#[derive(Default)]
struct Scheduler {
    generation: u64,
    preview_epoch: u64,
    preview_key: Option<String>,
    serial: u64,
    queued: HashMap<String, Request>,
    inflight: HashMap<String, Request>,
    ready: HashSet<String>,
    desired: HashMap<String, ThumbnailPriority>,
    prefetch: bool,
    closed: bool,
}
impl Scheduler {
    fn publish_preview_busy(&self) {
        performance::PREVIEW_BUSY.store(
            self.queued
                .values()
                .chain(self.inflight.values())
                .any(|r| r.kind == ImageKind::Preview),
            Ordering::Release,
        );
    }
    fn finish(&mut self, key: &str) {
        self.inflight.remove(key);
        self.publish_preview_busy();
    }
}
struct Shared {
    scheduler: Mutex<Scheduler>,
    changed: Condvar,
    workers: AtomicUsize,
    profile_path: Mutex<Option<PathBuf>>,
}
pub struct ThumbnailService {
    shared: Arc<Shared>,
    pub rx: Receiver<ImageResult>,
    pub preview_rx: Receiver<ImageResult>,
    pub cache: Arc<DiskCache>,
    decode_budget: Arc<ByteBudget>,
    ready_budget: Arc<ByteBudget>,
}
fn valid(s: &Scheduler, r: &Request) -> bool {
    !s.closed
        && s.generation == r.generation
        && if r.kind == ImageKind::Preview {
            s.preview_epoch == r.preview_epoch
                && s.preview_key.as_deref() == Some(&r.record.thumbnail_key)
        } else {
            s.desired.contains_key(&r.record.thumbnail_key)
        }
}
impl ThumbnailService {
    pub fn new(wakeup: Arc<dyn Fn() + Send + Sync>) -> Self {
        let shared = Arc::new(Shared {
            scheduler: Mutex::new(Scheduler {
                prefetch: true,
                ..Default::default()
            }),
            changed: Condvar::new(),
            workers: AtomicUsize::new(2),
            profile_path: Mutex::new(None),
        });
        let cache = DiskCache::new(wakeup.clone());
        let decode_budget =
            ByteBudget::new(performance::DECODE_BYTES, performance::PREVIEW_RESERVE);
        let ready_budget = ByteBudget::new(performance::READY_BYTES, 64 * MIB);
        let (result_tx, rx) = bounded(64);
        let (preview_tx, preview_rx) = bounded(1);
        let max = std::thread::available_parallelism()
            .map_or(2, |n| n.get())
            .saturating_sub(1)
            .clamp(1, 4);
        for index in 0..=max {
            let shared = shared.clone();
            let cache = cache.clone();
            let decode_budget = decode_budget.clone();
            let ready_budget = ready_budget.clone();
            let result_tx = if index == 0 {
                preview_tx.clone()
            } else {
                result_tx.clone()
            };
            let wakeup = wakeup.clone();
            thread::Builder::new()
                .name(if index == 0 {
                    "preview-decoder".into()
                } else {
                    format!("thumbnail-decoder-{index}")
                })
                .spawn(move || {
                    loop {
                        if index == 1 {
                            if let Some(path) = shared.profile_path.lock().take() {
                                shared.workers.store(
                                    crate::disk_profile::ordinary_workers(&path),
                                    Ordering::Release,
                                );
                            }
                        }
                        let request = {
                            let mut s = shared.scheduler.lock();
                            loop {
                                if s.closed {
                                    return;
                                }
                                let key = s
                                    .queued
                                    .iter()
                                    .filter(|(_, r)| {
                                        if index == 0 {
                                            r.kind == ImageKind::Preview
                                        } else {
                                            r.kind == ImageKind::Thumbnail
                                                && index <= shared.workers.load(Ordering::Acquire)
                                                && (r.priority != ThumbnailPriority::Prefetch
                                                    || s.prefetch)
                                        }
                                    })
                                    .min_by_key(|(_, r)| (r.priority, r.serial))
                                    .map(|(key, _)| key.clone());
                                if let Some(key) = key {
                                    let r = s.queued.remove(&key).unwrap();
                                    s.inflight.insert(key, r.clone());
                                    break r;
                                }
                                shared.changed.wait(&mut s);
                            }
                        };
                        performance::begin_request(request.generation, request.serial);
                        let cancelled = || !valid(&shared.scheduler.lock(), &request);
                        let result =
                            load(&request, &cache, &decode_budget, &ready_budget, &cancelled);
                        let key = request.key();
                        if cancelled() {
                            shared.scheduler.lock().finish(&key);
                            wakeup();
                            continue;
                        }
                        let output = match result {
                            Ok(output) => output,
                            Err(error) if error.is::<decoding::Cancelled>() => {
                                shared.scheduler.lock().finish(&key);
                                wakeup();
                                continue;
                            }
                            Err(error) => ImageResult {
                                generation: request.generation,
                                preview_epoch: request.preview_epoch,
                                record_id: request.record.id,
                                path: request.record.path.clone(),
                                modified_ns: request.record.modified_ns,
                                source_key: request.record.thumbnail_key.clone(),
                                texture_key: texture_key(&request.record, request.kind),
                                request_key: key.clone(),
                                kind: request.kind,
                                max_side: request.max_side,
                                pixels: vec![],
                                width: 0,
                                height: 0,
                                source_width: 0,
                                source_height: 0,
                                error: Some(error.to_string()),
                                failure: Some(if error.is::<decoding::ResourceLimit>() {
                                    FailureKind::ResourceLimit
                                } else {
                                    FailureKind::Decode
                                }),
                                _lease: None,
                            },
                        };
                        {
                            let mut s = shared.scheduler.lock();
                            let current = valid(&s, &request);
                            s.finish(&key);
                            // Cancellation and completion can race between decode and publication.
                            if !current {
                                drop(s);
                                drop(output);
                                wakeup();
                                continue;
                            }
                            s.ready.insert(key.clone());
                        }
                        let priority = shared
                            .scheduler
                            .lock()
                            .desired
                            .get(&request.record.thumbnail_key)
                            .copied()
                            .map_or(request.priority, |p| p.min(request.priority));
                        let sent = send_result(&result_tx, output, &cancelled, priority, &wakeup);
                        // ready remains until UI acknowledges, so a completed result is never redundantly decoded.
                        if !sent || cancelled() {
                            let mut s = shared.scheduler.lock();
                            if s.generation == request.generation
                                && (request.kind != ImageKind::Preview
                                    || s.preview_epoch == request.preview_epoch)
                            {
                                s.ready.remove(&key);
                            }
                            drop(s);
                            wakeup();
                        }
                    }
                })
                .expect("image worker thread");
        }
        Self {
            shared,
            rx,
            preview_rx,
            cache,
            decode_budget,
            ready_budget,
        }
    }
    pub fn set_generation(&mut self, generation: u64) {
        let mut s = self.shared.scheduler.lock();
        s.generation = generation;
        s.preview_epoch = s.preview_epoch.wrapping_add(1);
        s.queued.clear();
        s.ready.clear();
        s.desired.clear();
        s.preview_key = None;
        s.publish_preview_busy();
        drop(s);
        while let Ok(result) = self.rx.try_recv() {
            self.discard(result);
        }
        while let Ok(result) = self.preview_rx.try_recv() {
            self.discard(result);
        }
        self.shared.changed.notify_all();
    }
    pub fn set_root(&self, path: PathBuf) {
        *self.shared.profile_path.lock() = Some(path);
        self.shared.changed.notify_all();
    }
    pub fn sync_preview(&self, key: String) {
        if self.shared.scheduler.lock().preview_key.as_ref() != Some(&key) {
            self.begin_preview(Some(key));
        }
    }
    pub fn begin_preview(&self, key: Option<String>) {
        let mut s = self.shared.scheduler.lock();
        s.preview_epoch = s.preview_epoch.wrapping_add(1);
        s.preview_key = key;
        s.queued.retain(|_, r| r.kind != ImageKind::Preview);
        s.ready.retain(|k| !k.contains(":preview:"));
        s.publish_preview_busy();
        drop(s);
        while let Ok(result) = self.preview_rx.try_recv() {
            self.discard(result);
        }
        self.shared.changed.notify_all();
    }
    pub fn is_current(&self, r: &ImageResult) -> bool {
        let s = self.shared.scheduler.lock();
        s.generation == r.generation
            && if r.kind == ImageKind::Preview {
                s.preview_epoch == r.preview_epoch
                    && s.preview_key.as_deref() == Some(&r.source_key)
            } else {
                s.desired.contains_key(&r.source_key)
            }
    }
    pub fn discard(&self, result: ImageResult) {
        self.acknowledge(&result);
        let bytes = result.pixels.len();
        crate::retirement::retire(result, bytes);
    }
    pub fn acknowledge(&self, result: &ImageResult) {
        let mut s = self.shared.scheduler.lock();
        if s.generation == result.generation
            && (result.kind != ImageKind::Preview || s.preview_epoch == result.preview_epoch)
        {
            s.ready.remove(&result.request_key);
        }
    }
    pub fn set_viewport(&self, visible: Vec<String>, prefetch: Vec<String>, allow_prefetch: bool) {
        let mut s = self.shared.scheduler.lock();
        s.prefetch = allow_prefetch;
        s.desired.clear();
        if allow_prefetch {
            for key in prefetch {
                s.desired.insert(key, ThumbnailPriority::Prefetch);
            }
        }
        for key in visible {
            s.desired.insert(key, ThumbnailPriority::Visible);
        }
        let Scheduler {
            desired, queued, ..
        } = &mut *s;
        queued.retain(|_, r| {
            r.kind == ImageKind::Preview || desired.contains_key(&r.record.thumbnail_key)
        });
        for r in queued.values_mut() {
            if let Some(p) = desired.get(&r.record.thumbnail_key) {
                r.priority = *p;
            }
        }
        self.shared.changed.notify_all();
    }
    pub fn request_thumbnail(&self, record: ImageRecord, priority: ThumbnailPriority) {
        self.request(Arc::new(record), 256, ImageKind::Thumbnail, priority);
    }
    pub fn request_thumbnails(&self, records: Vec<(Arc<ImageRecord>, ThumbnailPriority)>) {
        if records.is_empty() {
            return;
        }
        let mut s = self.shared.scheduler.lock();
        let mut changed = false;
        for (record, priority) in records {
            changed |= self.queue_request(&mut s, record, 256, ImageKind::Thumbnail, priority);
        }
        drop(s);
        if changed {
            self.shared.changed.notify_all();
        }
    }
    pub fn request_preview(&self, record: Arc<ImageRecord>, max_side: u32) {
        // Generic codecs decode once to the full bounded preview rather than once per tier.
        let side = if matches!(record.format.as_str(), "jpg" | "jpeg") {
            max_side
        } else {
            4096
        };
        self.request(record, side, ImageKind::Preview, ThumbnailPriority::Preview);
    }
    pub fn clear_disk_cache(&self) -> anyhow::Result<()> {
        self.cache.clear();
        Ok(())
    }
    pub fn has_results(&self) -> bool {
        !self.rx.is_empty() || !self.preview_rx.is_empty()
    }
    pub fn record_metrics(&self) {
        if !performance::enabled() {
            return;
        }
        self.cache.record_metrics();
        let s = self.shared.scheduler.lock();
        performance::gauge("image_queued_count", s.queued.len() as f64);
        performance::gauge("image_inflight_count", s.inflight.len() as f64);
        performance::gauge("image_ready_count", s.ready.len() as f64);
        drop(s);
        performance::gauge("decode_budget_bytes", self.decode_budget.used() as f64);
        performance::gauge("ready_budget_bytes", self.ready_budget.used() as f64);
    }
    fn request(
        &self,
        record: Arc<ImageRecord>,
        max_side: u32,
        kind: ImageKind,
        priority: ThumbnailPriority,
    ) {
        let mut s = self.shared.scheduler.lock();
        let added = self.queue_request(&mut s, record, max_side, kind, priority);
        if added && kind == ImageKind::Preview {
            performance::PREVIEW_BUSY.store(true, Ordering::Release);
        }
        drop(s);
        if added {
            self.shared.changed.notify_all();
        }
    }
    fn queue_request(
        &self,
        s: &mut Scheduler,
        record: Arc<ImageRecord>,
        max_side: u32,
        kind: ImageKind,
        priority: ThumbnailPriority,
    ) -> bool {
        if kind == ImageKind::Preview && s.preview_key.as_deref() != Some(&record.thumbnail_key) {
            return false;
        }
        if kind == ImageKind::Thumbnail {
            s.desired
                .entry(record.thumbnail_key.clone())
                .or_insert(priority);
        }
        let key = request_key(&record, kind, max_side);
        if s.inflight.contains_key(&key) || s.ready.contains(&key) {
            return false;
        }
        if let Some(r) = s.queued.get_mut(&key) {
            r.priority = r.priority.min(priority);
            return false;
        }
        if s.queued.len() >= 256 {
            if let Some(evict) = s
                .queued
                .iter()
                .filter(|(_, r)| r.priority == ThumbnailPriority::Prefetch)
                .max_by_key(|(_, r)| r.serial)
                .map(|(k, _)| k.clone())
            {
                s.queued.remove(&evict);
            } else {
                return false;
            }
        }
        s.serial = s.serial.wrapping_add(1);
        let r = Request {
            generation: s.generation,
            preview_epoch: s.preview_epoch,
            cache_epoch: self.cache.epoch(),
            record,
            max_side,
            kind,
            priority,
            serial: s.serial,
        };
        s.queued.insert(key, r);
        true
    }
}
impl Drop for ThumbnailService {
    fn drop(&mut self) {
        self.shared.scheduler.lock().closed = true;
        self.shared.changed.notify_all();
    }
}
fn send_result(
    tx: &Sender<ImageResult>,
    mut output: ImageResult,
    cancel: &impl Fn() -> bool,
    priority: ThumbnailPriority,
    wakeup: &Arc<dyn Fn() + Send + Sync>,
) -> bool {
    loop {
        if cancel() {
            return false;
        }
        match tx.send_timeout(output, Duration::from_millis(10)) {
            Ok(()) => {
                wakeup();
                return true;
            }
            Err(crossbeam_channel::SendTimeoutError::Timeout(r)) => {
                if priority == ThumbnailPriority::Prefetch {
                    return false;
                }
                output = r;
            }
            Err(_) => return false,
        }
    }
}
pub fn texture_key(record: &ImageRecord, kind: ImageKind) -> String {
    format!(
        "{}:{}",
        record.thumbnail_key,
        if kind == ImageKind::Thumbnail {
            "thumb"
        } else {
            "preview"
        }
    )
}
fn request_key(record: &ImageRecord, kind: ImageKind, side: u32) -> String {
    format!("{}:{side}", texture_key(record, kind))
}
fn load(
    r: &Request,
    cache: &DiskCache,
    decode_budget: &Arc<ByteBudget>,
    ready_budget: &Arc<ByteBudget>,
    cancel: &impl Fn() -> bool,
) -> anyhow::Result<ImageResult> {
    let start = Instant::now();
    let preview = r.kind == ImageKind::Preview;
    let cached = if !preview {
        cache.read(&r.record.thumbnail_key)
    } else {
        None
    };
    performance::sample("cache_hit", if cached.is_some() { 1.0 } else { 0.0 });
    let write_cache = cached.as_ref().is_none_or(|(_, legacy)| *legacy);
    let (image, decode_lease) = if let Some((image, _)) = cached {
        (image, None)
    } else {
        let decoded = decoding::decode(&r.record, r.max_side, preview, decode_budget, cancel)?;
        (decoded.image, Some(decoded._lease))
    };
    if cancel() {
        return Err(decoding::Cancelled.into());
    }
    let image = Arc::new(image);
    let bytes = image.pixels.len();
    let lease = ready_budget
        .acquire(bytes, preview, cancel)
        .ok_or(decoding::Cancelled)?;
    let mut pixels = Vec::with_capacity(bytes);
    for pixel in image.pixels.chunks_exact(4) {
        pixels.extend_from_slice(
            &eframe::egui::Color32::from_rgba_unmultiplied(pixel[0], pixel[1], pixel[2], pixel[3])
                .to_array(),
        );
    }
    let result = ImageResult {
        generation: r.generation,
        preview_epoch: r.preview_epoch,
        record_id: r.record.id,
        path: r.record.path.clone(),
        modified_ns: r.record.modified_ns,
        source_key: r.record.thumbnail_key.clone(),
        texture_key: texture_key(&r.record, r.kind),
        request_key: r.key(),
        kind: r.kind,
        max_side: r.max_side,
        pixels,
        width: image.width,
        height: image.height,
        source_width: image.source_width,
        source_height: image.source_height,
        error: None,
        failure: None,
        _lease: Some(lease),
    };
    drop(decode_lease);
    if !preview && write_cache {
        cache.write(
            r.record.thumbnail_key.clone(),
            image,
            matches!(r.record.format.as_str(), "jpg" | "jpeg"),
            r.cache_epoch,
        );
    }
    performance::elapsed("image_ready_ms", start);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> ImageRecord {
        ImageRecord {
            id: 1,
            path: "missing.jpg".into(),
            relative_path: "missing.jpg".into(),
            file_name: "missing.jpg".into(),
            size: 1,
            modified_ns: 0,
            width: None,
            height: None,
            format: "jpg".into(),
            thumbnail_key: "a".repeat(64),
            content_hash: None,
        }
    }
    #[test]
    fn viewport_and_preview_cancel_old_work() {
        let mut s = Scheduler {
            generation: 3,
            preview_epoch: 2,
            preview_key: Some("a".repeat(64)),
            ..Default::default()
        };
        let mut r = Request {
            generation: 3,
            preview_epoch: 2,
            cache_epoch: 0,
            record: Arc::new(record()),
            max_side: 256,
            kind: ImageKind::Thumbnail,
            priority: ThumbnailPriority::Prefetch,
            serial: 1,
        };
        assert!(!valid(&s, &r));
        s.desired
            .insert(r.record.thumbnail_key.clone(), ThumbnailPriority::Visible);
        assert!(valid(&s, &r));
        r.kind = ImageKind::Preview;
        assert!(valid(&s, &r));
        s.preview_epoch += 1;
        assert!(!valid(&s, &r));
        s.generation += 1;
        r.kind = ImageKind::Thumbnail;
        assert!(!valid(&s, &r));
    }
    #[test]
    fn batch_deduplicates_and_shares_records() {
        let service = ThumbnailService::new(Arc::new(|| {}));
        let r = Arc::new(record());
        let mut scheduler = Scheduler::default();
        assert!(service.queue_request(
            &mut scheduler,
            r.clone(),
            256,
            ImageKind::Thumbnail,
            ThumbnailPriority::Prefetch
        ));
        assert!(!service.queue_request(
            &mut scheduler,
            r.clone(),
            256,
            ImageKind::Thumbnail,
            ThumbnailPriority::Visible
        ));
        assert_eq!(scheduler.queued.len(), 1);
        let task = scheduler.queued.values().next().unwrap();
        assert!(Arc::ptr_eq(&task.record, &r));
        assert_eq!(task.priority, ThumbnailPriority::Visible);
    }

    #[test]
    fn finishing_old_preview_keeps_disk_priority_for_the_new_request() {
        let mut s = Scheduler::default();
        let request = Request {
            generation: 1,
            preview_epoch: 1,
            cache_epoch: 0,
            record: Arc::new(record()),
            max_side: 1024,
            kind: ImageKind::Preview,
            priority: ThumbnailPriority::Preview,
            serial: 1,
        };
        s.inflight.insert("old".into(), request.clone());
        s.queued.insert(
            "new".into(),
            Request {
                preview_epoch: 2,
                ..request
            },
        );
        s.finish("old");
        assert!(performance::PREVIEW_BUSY.load(Ordering::Acquire));
        let newest = s.queued.remove("new").unwrap();
        s.inflight.insert("new".into(), newest);
        s.publish_preview_busy();
        assert!(performance::PREVIEW_BUSY.load(Ordering::Acquire));
        s.finish("new");
        assert!(!performance::PREVIEW_BUSY.load(Ordering::Acquire));
    }

    #[test]
    fn saturated_result_channel_can_be_cancelled_and_releases_lease() {
        let (tx, _rx) = bounded(0);
        let budget = ByteBudget::new(64, 0);
        let output = ImageResult {
            generation: 1,
            preview_epoch: 1,
            record_id: 1,
            path: "x".into(),
            modified_ns: 0,
            source_key: "x".into(),
            texture_key: "x".into(),
            request_key: "x".into(),
            kind: ImageKind::Preview,
            max_side: 1,
            pixels: vec![0; 4],
            width: 1,
            height: 1,
            source_width: 1,
            source_height: 1,
            error: None,
            failure: None,
            _lease: budget.try_acquire(4),
        };
        let started = Instant::now();
        let wakeup: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        assert!(!send_result(
            &tx,
            output,
            &|| started.elapsed() > Duration::from_millis(30),
            ThumbnailPriority::Preview,
            &wakeup
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(budget.used(), 0);
    }
}
