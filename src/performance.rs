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
    #[serde(skip)]
    flushed: Option<Sender<()>>,
}
static ENABLED: OnceLock<bool> = OnceLock::new();
static SAMPLES: OnceLock<Option<Sender<Sample>>> = OnceLock::new();
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
static FRAME: AtomicU64 = AtomicU64::new(0);
static SCENARIO: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);
thread_local! { static REQUEST: Cell<(u64, u64)> = const { Cell::new((0, 0)) }; }

pub fn begin_frame(scenario: u64) {
    FRAME.fetch_add(1, Ordering::Relaxed);
    SCENARIO.store(scenario, Ordering::Relaxed);
}
pub fn begin_request(generation: u64, request: u64) {
    REQUEST.set((generation, request));
}
fn event(name: &'static str, value: f64) -> Sample {
    let (generation, request_id) = REQUEST.get();
    Sample {
        time_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        monotonic_us: START.get_or_init(Instant::now).elapsed().as_micros(),
        qpc: qpc(),
        frame_id: FRAME.load(Ordering::Relaxed),
        scenario: SCENARIO.load(Ordering::Relaxed),
        request_id,
        generation,
        name,
        value,
        flushed: None,
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

pub fn sample(name: &'static str, value: f64) {
    if !enabled() {
        return;
    }
    let sender = SAMPLES.get_or_init(|| {
        if std::env::var_os("TUHAI_PERF").as_deref() != Some(std::ffi::OsStr::new("1")) {
            return None;
        }
        let (tx, rx) = bounded::<Sample>(4096);
        std::thread::Builder::new()
            .name("performance-log".into())
            .spawn(move || {
                use std::io::Write;
                // Diagnostic output can live on a spacious test volume; product data stays portable.
                let directory = std::env::var_os("TUHAI_PERF_LOG_DIR")
                    .map(std::path::PathBuf::from)
                    .map_or_else(crate::storage::data_dir, |dir| {
                        std::fs::create_dir_all(&dir)?;
                        Ok(dir)
                    });
                let Ok(dir) = directory else {
                    return;
                };
                let stamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                let path = dir.join(format!("performance-{stamp}.jsonl"));
                let Ok(file) = std::fs::File::create(&path)
                else {
                    return;
                };
                let mut file = std::io::BufWriter::new(file);
                let header = serde_json::json!({"schema": 3, "run_id": stamp, "pid": std::process::id(), "executable": std::env::current_exe().ok(), "scenario_name": std::env::var("TUHAI_PERF_SCENARIO").unwrap_or_else(|_| "trajectory".into()), "system_cache": "unknown", "kind": "run_header"});
                let _ = serde_json::to_writer(&mut file, &header);
                let _ = file.write_all(b"\n");
                let mut last_flush = Instant::now();
                loop {
                    match rx.recv_timeout(std::time::Duration::from_secs(1)) {
                        Ok(event) => {
                            if serde_json::to_writer(&mut file, &event).is_err() || file.write_all(b"\n").is_err() { return; }
                            if let Some(tx) = event.flushed {
                                if file.flush().is_ok() && file.get_ref().sync_data().is_ok() {
                                    // Publish completion only after the log has reached the filesystem.
                                    let certify = (|| -> std::io::Result<()> {
                                        let temporary = path.with_extension("complete.tmp");
                                        let mut certificate = std::fs::File::create(&temporary)?;
                                        let data = serde_json::json!({"run_id":stamp,"bytes":file.get_ref().metadata()?.len(),"sync_completed":true});
                                        certificate.write_all(data.to_string().as_bytes())?;
                                        certificate.sync_all()?;
                                        drop(certificate);
                                        std::fs::rename(temporary, path.with_extension("complete.json"))
                                    })();
                                    if certify.is_ok() { let _ = tx.try_send(()); }
                                }
                                return;
                            }
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                    }
                    if last_flush.elapsed().as_secs() >= 1 {
                        if let Some((private, working)) = process_memory() {
                            for (name, bytes) in [
                                ("process_private_bytes", private),
                                ("process_working_set_bytes", working),
                            ] {
                                let event = event(name, bytes as f64);
                                let _ = serde_json::to_writer(&mut file, &event);
                                let _ = file.write_all(b"\n");
                            }
                        }
                        let _ = file.flush();
                        last_flush = Instant::now();
                    }
                }
                let _ = file.flush();
            })
            .ok()?;
        Some(tx)
    });
    if let Some(tx) = sender {
        if tx.try_send(event(name, value)).is_err() {
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
}
/// Only called at process exit when diagnostics were explicitly enabled.
pub fn flush_at_exit() {
    if let Some(Some(tx)) = SAMPLES.get() {
        let (ack, rx) = bounded(1);
        let _ = tx.send_timeout(
            event("log_dropped", DROPPED.load(Ordering::Relaxed) as f64),
            std::time::Duration::from_millis(200),
        );
        let mut completed = event("log_flush", 1.0);
        completed.flushed = Some(ack);
        if tx
            .send_timeout(completed, std::time::Duration::from_millis(200))
            .is_ok()
        {
            let _ = rx.recv_timeout(std::time::Duration::from_millis(500));
        }
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
