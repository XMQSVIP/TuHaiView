//! Central resource policy and opt-in, asynchronous local performance samples.
use crossbeam_channel::{Sender, bounded};
use serde::{Deserialize, Serialize};
use std::{
    sync::OnceLock,
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
    name: &'static str,
    value: f64,
    #[serde(skip)]
    flushed: Option<Sender<()>>,
}
static SAMPLES: OnceLock<Option<Sender<Sample>>> = OnceLock::new();

pub fn sample(name: &'static str, value: f64) {
    let sender = SAMPLES.get_or_init(|| {
        if std::env::var_os("TUHAI_PERF").as_deref() != Some(std::ffi::OsStr::new("1")) {
            return None;
        }
        let (tx, rx) = bounded::<Sample>(4096);
        std::thread::Builder::new()
            .name("performance-log".into())
            .spawn(move || {
                use std::io::Write;
                let Ok(dir) = crate::storage::data_dir() else {
                    return;
                };
                let stamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                let Ok(file) =
                    std::fs::File::create(dir.join(format!("performance-{stamp}.jsonl")))
                else {
                    return;
                };
                let mut file = std::io::BufWriter::new(file);
                let mut last_flush = Instant::now();
                loop {
                    match rx.recv_timeout(std::time::Duration::from_secs(1)) {
                        Ok(event) => {
                            let _ = serde_json::to_writer(&mut file, &event);
                            let _ = file.write_all(b"\n");
                            if let Some(tx) = event.flushed {
                                let _ = file.flush();
                                let _ = tx.try_send(());
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
                                let event = Sample {
                                    time_ms: SystemTime::now()
                                        .duration_since(UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_millis(),
                                    name,
                                    value: bytes as f64,
                                    flushed: None,
                                };
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
        let _ = tx.try_send(Sample {
            time_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            name,
            value,
            flushed: None,
        });
    }
}
/// Only called at process exit when diagnostics were explicitly enabled.
pub fn flush_at_exit() {
    if let Some(Some(tx)) = SAMPLES.get() {
        let (ack, rx) = bounded(1);
        if tx
            .try_send(Sample {
                time_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
                name: "log_flush",
                value: 0.0,
                flushed: Some(ack),
            })
            .is_ok()
        {
            let _ = rx.recv_timeout(std::time::Duration::from_millis(200));
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
}
impl UiRun {
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
