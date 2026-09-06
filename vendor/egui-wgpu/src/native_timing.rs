//! TuHaiView opt-in CPU phase observer. No clock calls when no observer is installed.
use std::sync::OnceLock;
use web_time::Instant;

pub const NATIVE_PHASES: [&str; 14] = [
    "encoder_create_ms",
    "upload_lock_ms",
    "mesh_recall_ms",
    "egui_texture_upload_ms",
    "mesh_upload_ms",
    "surface_acquire_ms",
    "render_lock_ms",
    "render_encode_ms",
    "encoder_finish_ms",
    "queue_submit_ms",
    "release_lock_ms",
    "egui_texture_free_ms",
    "capture_ms",
    "present_ms",
];

#[derive(Clone, Copy, Debug)]
pub struct NativeFrameTimings {
    pub phases_ms: [f64; 14],
    pub total_ms: f64,
    pub presented: bool,
}

/// Captured once at paint entry; completion is called after all renderer locks drop.
pub struct NativeFrameObserver {
    pub begin: fn() -> [u64; 3],
    pub end: fn([u64; 3], NativeFrameTimings),
}
static OBSERVER: OnceLock<NativeFrameObserver> = OnceLock::new();

pub fn install_native_frame_observer(observer: NativeFrameObserver) -> bool {
    OBSERVER.set(observer).is_ok()
}

pub(crate) struct FrameScope {
    observer: Option<&'static NativeFrameObserver>,
    context: [u64; 3],
    started: Option<Instant>,
    last: Option<Instant>,
    timings: NativeFrameTimings,
}
impl FrameScope {
    pub fn new(root: bool) -> Self {
        let observer = OBSERVER.get().filter(|_| root);
        let context = observer.map_or([0; 3], |o| (o.begin)());
        let started = observer.map(|_| Instant::now());
        Self {
            observer,
            context,
            started,
            last: started,
            timings: NativeFrameTimings {
                phases_ms: [0.0; 14],
                total_ms: 0.0,
                presented: false,
            },
        }
    }
    pub fn lap(&mut self, index: usize) {
        if let Some(last) = self.last {
            let now = Instant::now();
            self.timings.phases_ms[index] += now.duration_since(last).as_secs_f64() * 1000.0;
            self.last = Some(now);
        }
    }
    pub fn presented(&mut self) {
        self.timings.presented = true;
    }
}
impl Drop for FrameScope {
    fn drop(&mut self) {
        if let (Some(observer), Some(started)) = (self.observer, self.started) {
            self.timings.total_ms = started.elapsed().as_secs_f64() * 1000.0;
            (observer.end)(self.context, self.timings);
        }
    }
}
