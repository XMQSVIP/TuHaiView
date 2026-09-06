//! Central resource policy and opt-in, asynchronous local performance samples.
use crossbeam_channel::{Sender, bounded};
use serde::{Deserialize, Serialize};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};
mod logging;
pub use logging::FinalizeOutcome;

pub const MIB: usize = 1024 * 1024;
pub const EVENT_BUDGET_MS: u64 = 2;
pub const UPLOAD_BUDGET_MS: u64 = 2;
pub const UPLOAD_BYTES: usize = 4 * MIB;
pub const THUMBNAILS_PER_FRAME: usize = 8;
pub const DECODE_BYTES: usize = 512 * MIB;
pub const PREVIEW_RESERVE: usize = 128 * MIB;
pub const READY_BYTES: usize = 96 * MIB;
pub const CACHE_QUEUE_BYTES: usize = 32 * MIB;
pub const TEXTURE_BYTES: usize = 256 * MIB;

/// Process-local timer experiment, paired on shutdown; disabled in ordinary use.
pub struct TimerResolution(bool);
impl TimerResolution {
    pub fn diagnostic() -> Self {
        #[cfg(windows)]
        if enabled() && std::env::var("TUHAI_PERF_TIMER_MS").ok().as_deref() == Some("1") {
            let active = unsafe { timeBeginPeriod(1) } == 0;
            sample("timer_resolution_1ms", active as u8 as f64);
            return Self(active);
        }
        Self(false)
    }
}
impl Drop for TimerResolution {
    fn drop(&mut self) {
        #[cfg(windows)]
        if self.0 {
            unsafe {
                timeEndPeriod(1);
            }
        }
    }
}
#[cfg(windows)]
#[link(name = "winmm")]
unsafe extern "system" {
    fn timeBeginPeriod(period: u32) -> u32;
    fn timeEndPeriod(period: u32) -> u32;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PerformanceSettings {
    pub disk_cache_gib: u64,
}
impl Default for PerformanceSettings {
    fn default() -> Self {
        Self { disk_cache_gib: 1 }
    }
}
impl PerformanceSettings {
    pub fn validate(&mut self) {
        if ![1, 2, 4, 8, 16].contains(&self.disk_cache_gib) {
            self.disk_cache_gib = 1;
        }
    }
}

#[derive(Serialize)]
struct Sample {
    time_ms: u128,
    monotonic_us: u128,
    qpc: i64,
    frame_id: u64,
    scenario: u64,
    request_id: u64,
    generation: u64,
    name: &'static str,
    value: f64,
    frame_known: bool,
}
static ENABLED: OnceLock<bool> = OnceLock::new();
static LOGGER: OnceLock<logging::Logger> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();
pub fn initialize_clock() {
    if enabled() {
        START.get_or_init(Instant::now);
    }
}
pub fn since_start(name: &'static str) {
    if enabled() {
        static PUBLISHED: OnceLock<std::sync::Mutex<std::collections::HashSet<&'static str>>> =
            OnceLock::new();
        if PUBLISHED
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .insert(name)
        {
            sample(
                name,
                START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0,
            );
        }
    }
}
static SCENARIO: AtomicU64 = AtomicU64::new(8);
static FRAME: AtomicU64 = AtomicU64::new(0);
#[derive(Clone, Copy, Default, Debug, Serialize)]
pub struct FrameContext {
    pub frame_id: u64,
    pub scenario: u64,
    pub known: bool,
}
thread_local! {
    static FRAME_CONTEXT: Cell<FrameContext> = Cell::new(FrameContext::default());
    static PREVIOUS_FRAME: Cell<FrameContext> = Cell::new(FrameContext::default());
}
thread_local! { static REQUEST: Cell<(u64, u64)> = const { Cell::new((0, 0)) }; }

pub fn begin_frame(scenario: u64) {
    if enabled() {
        SCENARIO.store(scenario, Ordering::Relaxed);
        PREVIOUS_FRAME.set(FRAME_CONTEXT.get());
        FRAME_CONTEXT.set(FrameContext {
            frame_id: FRAME.fetch_add(1, Ordering::Relaxed) + 1,
            scenario,
            known: true,
        });
    }
}
pub fn frame_context() -> FrameContext {
    FRAME_CONTEXT.get()
}
pub fn previous_frame_cpu(seconds: f32) {
    if enabled() {
        // eframe reports the previous update/render cycle. The first cycle is unknown.
        logger().push(cpu_event(seconds, PREVIOUS_FRAME.get()));
    }
}
fn cpu_event(seconds: f32, frame: FrameContext) -> Sample {
    let mut sample = event("eframe_cpu_ms", seconds as f64 * 1000.0);
    sample.set_frame(frame);
    sample
}
impl Sample {
    fn set_frame(&mut self, frame: FrameContext) {
        self.frame_id = frame.frame_id;
        self.scenario = if frame.known { frame.scenario } else { 8 };
        self.frame_known = frame.known;
    }
}
pub fn begin_request(generation: u64, request: u64) {
    REQUEST.set((generation, request));
}
fn event(name: &'static str, value: f64) -> Sample {
    let (generation, request_id) = REQUEST.get();
    let frame = frame_context();
    Sample {
        time_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        monotonic_us: START.get_or_init(Instant::now).elapsed().as_micros(),
        qpc: qpc(),
        frame_id: frame.frame_id,
        scenario: if frame.known {
            frame.scenario
        } else {
            SCENARIO.load(Ordering::Relaxed)
        },
        request_id,
        generation,
        name,
        value,
        frame_known: frame.known,
    }
}
#[cfg(windows)]
fn qpc() -> i64 {
    let mut value = 0;
    unsafe {
        let _ = windows::Win32::System::Performance::QueryPerformanceCounter(&mut value);
    }
    value
}
#[cfg(not(windows))]
fn qpc() -> i64 {
    0
}

pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var("TUHAI_PERF").ok().as_deref() == Some("1"))
}
thread_local! { static GAUGES: RefCell<HashMap<&'static str, (Instant, f64)>> = RefCell::new(HashMap::new()); }
thread_local! { static NATIVE_DIALOG_TIME: Cell<std::time::Duration> = const { Cell::new(std::time::Duration::ZERO) }; }

/// Keep time spent waiting for native modal input distinct from UI processing.
/// Disabled in normal use; the dialog and its result are otherwise untouched.
pub fn native_dialog<T>(open: impl FnOnce() -> T) -> T {
    if !enabled() {
        return open();
    }
    let started = Instant::now();
    sample("native_dialog_open", 1.0);
    let result = open();
    let waited = started.elapsed();
    NATIVE_DIALOG_TIME.set(NATIVE_DIALOG_TIME.get().saturating_add(waited));
    sample("native_dialog_wait_ms", waited.as_secs_f64() * 1000.0);
    result
}

pub fn native_dialog_time() -> std::time::Duration {
    NATIVE_DIALOG_TIME.get()
}

/// Resource/config gauges need neither a record nor a scheduler lock every frame.
pub fn gauge(name: &'static str, value: f64) {
    if !enabled() {
        return;
    }
    let publish = GAUGES.with(|values| {
        let mut values = values.borrow_mut();
        let now = Instant::now();
        let changed = values
            .get(name)
            .is_none_or(|(last, old)| *old != value || last.elapsed().as_secs() >= 1);
        if changed {
            values.insert(name, (now, value));
        }
        changed
    });
    if publish {
        sample(name, value);
    }
}

fn logger() -> &'static logging::Logger {
    LOGGER.get_or_init(logging::Logger::start)
}
pub fn sample(name: &'static str, value: f64) {
    if enabled() {
        logger().push(event(name, value));
    }
}
/// UI only closes the producer gate and sends a separate control message.
pub fn request_finish() {
    if let Some(logger) = LOGGER.get() {
        logger.request_finish();
    }
}
/// Called after run_native has returned; never from the UI event loop.
pub fn finalize_after_window() -> FinalizeOutcome {
    LOGGER
        .get()
        .map_or(FinalizeOutcome::Disabled, logging::Logger::wait)
}

#[cfg(test)]
mod frame_tests {
    use super::*;
    #[test]
    fn previous_cpu_preserves_source_frame_and_marks_unknown() {
        let sample = cpu_event(
            0.02,
            FrameContext {
                frame_id: 41,
                scenario: 1,
                known: true,
            },
        );
        assert_eq!(
            (sample.frame_id, sample.scenario, sample.frame_known),
            (41, 1, true)
        );
        let sample = cpu_event(0.02, FrameContext::default());
        assert_eq!(
            (sample.frame_id, sample.scenario, sample.frame_known),
            (0, 8, false)
        );
    }
}

pub fn elapsed(name: &'static str, start: Instant) {
    sample(name, start.elapsed().as_secs_f64() * 1000.0);
}

#[cfg(windows)]
fn process_memory() -> Option<(usize, usize)> {
    use windows::Win32::System::{
        ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
        },
        Threading::GetCurrentProcess,
    };
    let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
    counters.cb = std::mem::size_of_val(&counters) as u32;
    unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        )
        .ok()?;
    }
    Some((counters.PrivateUsage, counters.WorkingSetSize))
}
#[cfg(not(windows))]
fn process_memory() -> Option<(usize, usize)> {
    None
}

pub static PREVIEW_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub struct UiRun {
    pub root: std::path::PathBuf,
    pub seconds: f64,
    pub started: Instant,
    pub last_sort: u64,
    pub captures: u8,
    pub scenario: String,
    pub last_root_switch: u64,
    pub alternate_root: Option<std::path::PathBuf>,
    pub route_started: Option<Instant>,
}
impl UiRun {
    pub fn phase(&self) -> u64 {
        if self.scenario == "open" {
            return 6;
        }
        if self.scenario == "idle"
            || self.seconds - self.started.elapsed().as_secs_f64() <= 30.0
                && self.scenario == "soak"
        {
            return 7;
        }
        if self.scenario == "scroll" {
            return if self
                .route_started
                .is_none_or(|s| s.elapsed().as_secs_f64() < 20.0)
            {
                0
            } else {
                1
            };
        }
        (self.started.elapsed().as_secs() % 60) / 10
    }
    pub fn from_environment() -> Option<Self> {
        if std::env::var("TUHAI_PERF").ok().as_deref() != Some("1") {
            return None;
        }
        let root = std::path::PathBuf::from(std::env::var_os("TUHAI_PERF_ROOT")?);
        if !root.is_dir() {
            return None;
        }
        Some(Self {
            root,
            seconds: std::env::var("TUHAI_PERF_SECONDS")
                .ok()?
                .parse::<f64>()
                .ok()?
                .clamp(10.0, 7200.0),
            started: Instant::now(),
            last_sort: u64::MAX,
            captures: 0,
            scenario: std::env::var("TUHAI_PERF_SCENARIO").unwrap_or_else(|_| "trajectory".into()),
            last_root_switch: 0,
            route_started: None,
            alternate_root: std::env::var_os("TUHAI_PERF_ALTERNATE_ROOT")
                .map(std::path::PathBuf::from)
                .filter(|p| p.is_dir()),
        })
    }
}

// Render-output QA: eframe supplies only this application's rendered framebuffer.
pub fn save_capture(image: std::sync::Arc<eframe::egui::ColorImage>, name: String) {
    if std::env::var("TUHAI_PERF_CAPTURE").ok().as_deref() != Some("1") {
        return;
    }
    static CAPTURES: OnceLock<Sender<(std::sync::Arc<eframe::egui::ColorImage>, String)>> =
        OnceLock::new();
    let tx = CAPTURES.get_or_init(|| {
        let (tx, rx) = bounded::<(std::sync::Arc<eframe::egui::ColorImage>, String)>(2);
        std::thread::spawn(move || {
            while let Ok((image, name)) = rx.recv() {
                if let Ok(dir) = crate::storage::data_dir() {
                    let bytes: Vec<_> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                    let _ = image::save_buffer(
                        dir.join(format!("qa-{name}.png")),
                        &bytes,
                        image.size[0] as u32,
                        image.size[1] as u32,
                        image::ColorType::Rgba8,
                    );
                }
            }
        });
        tx
    });
    let _ = tx.try_send((image, name));
}
