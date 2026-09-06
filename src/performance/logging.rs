//! Bounded samples, a separate shutdown channel, and an explicit producer barrier.
use super::{Sample, event};
use crossbeam_channel::{Receiver, Sender, bounded};
use parking_lot::Mutex;
use serde::Serialize;
use std::{
    io::{self, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const DEADLINE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Starting,
    Receiving,
    Draining,
    LogWrite,
    LogFlush,
    LogSync,
    CertificateWrite,
    CertificateSync,
    Rename,
    Complete,
}
impl Stage {
    fn from_byte(value: u8) -> Self {
        [
            Self::Starting,
            Self::Receiving,
            Self::Draining,
            Self::LogWrite,
            Self::LogFlush,
            Self::LogSync,
            Self::CertificateWrite,
            Self::CertificateSync,
            Self::Rename,
            Self::Complete,
        ][value as usize]
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FinalizeOutcome {
    Disabled,
    Complete {
        elapsed_ms: f64,
        accepted: u64,
        dropped: u64,
        stages_ms: Vec<(Stage, f64)>,
    },
    Timeout {
        stage: Stage,
        elapsed_ms: f64,
    },
    Failed {
        stage: Stage,
        elapsed_ms: f64,
        error: String,
        os_error: Option<i32>,
    },
}
impl FinalizeOutcome {
    pub fn succeeded(&self) -> bool {
        matches!(self, Self::Disabled | Self::Complete { dropped: 0, .. })
    }
}
struct Gate {
    finish: Option<Instant>,
    accepted: u64,
    dropped: u64,
}
#[derive(Clone, Copy)]
struct Finish {
    started: Instant,
    accepted: u64,
    dropped: u64,
}
type Hook = Arc<dyn Fn(Stage) -> io::Result<()> + Send + Sync>;
pub(super) struct Logger {
    samples: Sender<Sample>,
    control: Sender<Finish>,
    done: Receiver<FinalizeOutcome>,
    gate: Mutex<Gate>,
    result: Mutex<Option<FinalizeOutcome>>,
    stage: Arc<AtomicU8>,
    deadline: Duration,
}
impl Logger {
    pub fn start() -> Self {
        let directory = std::env::var_os("TUHAI_PERF_LOG_DIR")
            .map(PathBuf::from)
            .map_or_else(
                || {
                    std::env::current_exe().and_then(|p| {
                        p.parent()
                            .map(|d| d.join("data"))
                            .ok_or_else(|| io::Error::other("executable has no parent"))
                    })
                },
                Ok,
            );
        Self::spawn(directory, 4096, DEADLINE, Arc::new(|_| Ok(())))
    }
    fn spawn(
        directory: io::Result<PathBuf>,
        capacity: usize,
        deadline: Duration,
        hook: Hook,
    ) -> Self {
        let (samples, rx) = bounded(capacity);
        let (control, stop) = bounded(1);
        let (reply, done) = bounded(1);
        let stage = Arc::new(AtomicU8::new(Stage::Starting as u8));
        let progress = stage.clone();
        let failed_spawn = reply.clone();
        let worker = std::thread::Builder::new()
            .name("performance-log".into())
            .spawn(move || {
                let started = Instant::now();
                let mut output = Writer {
                    stage: progress,
                    hook,
                    timings: Vec::new(),
                    deadline,
                    finish: None,
                };
                let result = output.run(directory, rx, stop);
                let outcome = match result {
                    Ok(finish) => FinalizeOutcome::Complete {
                        elapsed_ms: finish.started.elapsed().as_secs_f64() * 1000.0,
                        accepted: finish.accepted,
                        dropped: finish.dropped,
                        stages_ms: output.timings,
                    },
                    Err(error) => {
                        let stage = Stage::from_byte(output.stage.load(Ordering::Acquire));
                        let elapsed_ms = output
                            .finish
                            .map_or(started, |f| f.started)
                            .elapsed()
                            .as_secs_f64()
                            * 1000.0;
                        if error.kind() == io::ErrorKind::TimedOut {
                            FinalizeOutcome::Timeout { stage, elapsed_ms }
                        } else {
                            FinalizeOutcome::Failed {
                                stage,
                                elapsed_ms,
                                os_error: error.raw_os_error(),
                                error: error.to_string(),
                            }
                        }
                    }
                };
                let _ = reply.send(outcome);
            });
        if let Err(error) = worker {
            let _ = failed_spawn.send(FinalizeOutcome::Failed {
                stage: Stage::Starting,
                elapsed_ms: 0.0,
                os_error: error.raw_os_error(),
                error: error.to_string(),
            });
        }
        Self {
            samples,
            control,
            done,
            gate: Mutex::new(Gate {
                finish: None,
                accepted: 0,
                dropped: 0,
            }),
            result: Mutex::new(None),
            stage,
            deadline,
        }
    }
    pub fn push(&self, sample: Sample) {
        let mut gate = self.gate.lock();
        if gate.finish.is_some() {
            return;
        }
        if self.samples.try_send(sample).is_ok() {
            gate.accepted += 1;
        } else {
            gate.dropped += 1;
        }
    }
    pub fn request_finish(&self) {
        // No producer can enqueue after this barrier. No I/O, draining or joining here.
        let mut gate = self.gate.lock();
        if gate.finish.is_none() {
            let started = Instant::now();
            gate.finish = Some(started);
            let _ = self.control.try_send(Finish {
                started,
                accepted: gate.accepted,
                dropped: gate.dropped,
            });
        }
    }
    pub fn wait(&self) -> FinalizeOutcome {
        self.request_finish();
        let mut result = self.result.lock();
        if let Some(value) = result.as_ref() {
            return value.clone();
        }
        let started = self.gate.lock().finish.unwrap();
        let remaining = self.deadline.saturating_sub(started.elapsed());
        let value = self.done.recv_timeout(remaining).unwrap_or_else(|error| {
            let stage = Stage::from_byte(self.stage.load(Ordering::Acquire));
            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
            if error == crossbeam_channel::RecvTimeoutError::Timeout {
                FinalizeOutcome::Timeout { stage, elapsed_ms }
            } else {
                FinalizeOutcome::Failed {
                    stage,
                    elapsed_ms,
                    error: "logger disconnected".into(),
                    os_error: None,
                }
            }
        });
        *result = Some(value.clone());
        value
    }
}
struct Writer {
    stage: Arc<AtomicU8>,
    hook: Hook,
    timings: Vec<(Stage, f64)>,
    deadline: Duration,
    finish: Option<Finish>,
}
impl Writer {
    fn check_deadline(&self) -> io::Result<()> {
        if self
            .finish
            .is_some_and(|f| f.started.elapsed() >= self.deadline)
        {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "diagnostic finalization deadline exceeded",
            ));
        }
        Ok(())
    }
    fn step<T>(&mut self, stage: Stage, work: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        self.stage.store(stage as u8, Ordering::Release);
        self.check_deadline()?;
        let started = Instant::now();
        (self.hook)(stage)?;
        self.check_deadline()?;
        let value = work()?;
        // Only finalization stages need persistent timing entries.
        if self.finish.is_some() {
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            if let Some((_, total)) = self.timings.iter_mut().find(|(s, _)| *s == stage) {
                *total += ms;
            } else {
                self.timings.push((stage, ms));
            }
        }
        self.check_deadline()?;
        Ok(value)
    }
    fn line(&mut self, file: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
        self.step(Stage::LogWrite, || {
            serde_json::to_writer(&mut *file, value)?;
            file.write_all(b"\n")
        })
    }
    fn run(
        &mut self,
        directory: io::Result<PathBuf>,
        samples: Receiver<Sample>,
        stop: Receiver<Finish>,
    ) -> io::Result<Finish> {
        let dir = directory?;
        let stamp = format!(
            "{}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            std::process::id()
        );
        let path = dir.join(format!("performance-{stamp}.jsonl"));
        let raw = self.step(Stage::Starting, || {
            std::fs::create_dir_all(&dir)?;
            std::fs::File::create(&path)
        })?;
        let mut file = io::BufWriter::new(raw);
        self.line(&mut file, &serde_json::json!({"schema":4,"kind":"run_header","run_id":stamp,"pid":std::process::id(),"executable":std::env::current_exe().ok(),"scenario_name":std::env::var("TUHAI_PERF_SCENARIO").unwrap_or_else(|_|"trajectory".into()),"system_cache":"unknown"}))?;
        let mut last_flush = Instant::now();
        let mut written = 0u64;
        let finish = loop {
            self.stage.store(Stage::Receiving as u8, Ordering::Release);
            crossbeam_channel::select_biased! {
                recv(stop) -> finish => break finish.map_err(|_| io::Error::other("shutdown channel disconnected"))?,
                recv(samples) -> sample => {
                    let sample = sample.map_err(|_| io::Error::other("sample channel disconnected"))?;
                    self.line(&mut file, &sample)?;
                    written += 1;
                }
                default(Duration::from_millis(100)) => {}
            }
            if last_flush.elapsed() >= Duration::from_secs(1) {
                if let Some((private, working)) = super::process_memory() {
                    for (name, bytes) in [
                        ("process_private_bytes", private),
                        ("process_working_set_bytes", working),
                    ] {
                        self.line(&mut file, &event(name, bytes as f64))?;
                    }
                }
                self.step(Stage::LogFlush, || file.flush())?;
                last_flush = Instant::now();
            }
        };
        self.finish = Some(finish);
        self.stage.store(Stage::Draining as u8, Ordering::Release);
        for sample in samples.try_iter() {
            self.line(&mut file, &sample)?;
            written += 1;
        }
        if written != finish.accepted {
            return Err(io::Error::other("accepted/drained sample count mismatch"));
        }
        self.line(&mut file, &event("log_accepted", finish.accepted as f64))?;
        self.line(&mut file, &event("log_dropped", finish.dropped as f64))?;
        self.line(&mut file, &event("log_flush", 1.0))?;
        self.step(Stage::LogFlush, || file.flush())?;
        self.step(Stage::LogSync, || file.get_ref().sync_all())?;
        let temporary = path.with_extension("complete.tmp");
        let certificate = serde_json::json!({"run_id":stamp,"bytes":file.get_ref().metadata()?.len(),"sync_completed":true,"accepted":finish.accepted,"written":written,"dropped":finish.dropped,"stages_ms":self.timings});
        let mut cert = self.step(Stage::CertificateWrite, || {
            let mut cert = std::fs::File::create(&temporary)?;
            cert.write_all(certificate.to_string().as_bytes())?;
            Ok(cert)
        })?;
        self.step(Stage::CertificateSync, || {
            cert.flush()?;
            cert.sync_all()
        })?;
        drop(cert);
        self.step(Stage::Rename, || {
            std::fs::rename(&temporary, path.with_extension("complete.json"))
        })?;
        self.stage.store(Stage::Complete as u8, Ordering::Release);
        Ok(finish)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self(
                std::env::temp_dir().join(format!(
                    "tuhai-logger-{}-{}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                )),
            )
        }
        fn log(&self) -> PathBuf {
            std::fs::read_dir(&self.0)
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| p.extension().is_some_and(|e| e == "jsonl"))
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn normal() -> Hook {
        Arc::new(|_| Ok(()))
    }
    #[test]
    fn logger_saturation_has_independent_stop_and_no_samples_after_terminal() {
        let fixture = Fixture::new();
        let (release, wait) = bounded(1);
        let log = Logger::spawn(
            Ok(fixture.0.clone()),
            2,
            DEADLINE,
            Arc::new(move |stage| {
                if stage == Stage::Starting {
                    wait.recv().unwrap();
                }
                Ok(())
            }),
        );
        for i in 0..8 {
            log.push(event("test", i as f64));
        }
        log.request_finish();
        log.request_finish();
        log.push(event("late", 1.0));
        release.send(()).unwrap();
        assert!(matches!(
            log.wait(),
            FinalizeOutcome::Complete {
                accepted: 2,
                dropped: 6,
                ..
            }
        ));
        let path = fixture.log();
        let lines: Vec<serde_json::Value> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(lines.last().unwrap()["name"], "log_flush");
        assert!(!lines.iter().any(|s| s["name"] == "late"));
        let cert: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.with_extension("complete.json")).unwrap())
                .unwrap();
        assert_eq!(cert["accepted"], 2);
        assert_eq!(cert["bytes"], std::fs::metadata(path).unwrap().len());
    }
    #[test]
    fn logger_concurrent_producers_are_drained_exactly_once() {
        let fixture = Fixture::new();
        let log = Arc::new(Logger::spawn(
            Ok(fixture.0.clone()),
            4096,
            DEADLINE,
            normal(),
        ));
        let barrier = Arc::new(std::sync::Barrier::new(5));
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let log = log.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    for _ in 0..1000 {
                        log.push(event("test", 1.0));
                    }
                })
            })
            .collect();
        barrier.wait();
        log.push(event("test", 1.0));
        log.request_finish();
        for worker in workers {
            worker.join().unwrap();
        }
        let outcome = log.wait();
        let FinalizeOutcome::Complete {
            accepted, dropped, ..
        } = outcome
        else {
            panic!("{outcome:?}")
        };
        let lines = std::fs::read_to_string(fixture.log()).unwrap();
        assert_eq!(
            lines
                .lines()
                .filter(|s| s.contains("\"name\":\"test\""))
                .count() as u64,
            accepted
        );
        assert_eq!(dropped, 0);
    }
    #[test]
    fn logger_slow_sync_exceeds_old_timeout_but_finishes() {
        let fixture = Fixture::new();
        let log = Logger::spawn(
            Ok(fixture.0.clone()),
            16,
            DEADLINE,
            Arc::new(|s| {
                if s == Stage::LogSync {
                    std::thread::sleep(Duration::from_millis(650));
                }
                Ok(())
            }),
        );
        log.push(event("test", 1.0));
        let started = Instant::now();
        log.request_finish();
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(log.wait().succeeded());
        assert!(started.elapsed() >= Duration::from_millis(650));
        assert!(fixture.log().with_extension("complete.json").exists());
        assert!(log.wait().succeeded());
    }
    #[test]
    fn logger_timeout_stays_invalid_and_never_certifies_after_delay() {
        let fixture = Fixture::new();
        let (finished, ack) = bounded(1);
        let log = Logger::spawn(
            Ok(fixture.0.clone()),
            16,
            Duration::from_millis(50),
            Arc::new(move |s| {
                if s == Stage::LogSync {
                    std::thread::sleep(Duration::from_millis(150));
                    let _ = finished.send(());
                }
                Ok(())
            }),
        );
        log.request_finish();
        assert!(matches!(
            log.wait(),
            FinalizeOutcome::Timeout {
                stage: Stage::LogSync,
                ..
            }
        ));
        ack.recv_timeout(Duration::from_secs(2)).unwrap();
        let _ = log.done.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!fixture.log().with_extension("complete.json").exists());
        assert!(matches!(log.wait(), FinalizeOutcome::Timeout { .. }));
    }
    #[test]
    fn logger_write_sync_and_rename_failures_preserve_stage_and_error() {
        for target in [
            Stage::LogWrite,
            Stage::LogFlush,
            Stage::LogSync,
            Stage::CertificateWrite,
            Stage::CertificateSync,
            Stage::Rename,
        ] {
            let fixture = Fixture::new();
            let log = Logger::spawn(
                Ok(fixture.0.clone()),
                16,
                DEADLINE,
                Arc::new(move |s| {
                    if s == target {
                        Err(io::Error::from_raw_os_error(5))
                    } else {
                        Ok(())
                    }
                }),
            );
            log.push(event("test", 1.0));
            let outcome = log.wait();
            assert!(
                matches!(outcome,FinalizeOutcome::Failed{stage,os_error:Some(5),..} if stage==target),
                "{outcome:?}"
            );
            assert!(!fixture.log().with_extension("complete.json").exists());
            if target == Stage::Rename {
                assert!(fixture.log().with_extension("complete.tmp").exists());
            }
        }
    }
    #[test]
    fn logger_initialization_failure_is_observable() {
        let log = Logger::spawn(Err(io::Error::from_raw_os_error(5)), 1, DEADLINE, normal());
        assert!(matches!(
            log.wait(),
            FinalizeOutcome::Failed {
                stage: Stage::Starting,
                os_error: Some(5),
                ..
            }
        ));
    }
}
