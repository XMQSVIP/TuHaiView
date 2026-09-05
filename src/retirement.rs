//! CPU-only reclamation must not become a frame-time spike on the UI thread.
use crossbeam_channel::{Sender, unbounded};
use std::sync::{
    OnceLock,
    atomic::{AtomicUsize, Ordering},
};

static QUEUED: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
struct Retired {
    value: Box<dyn Send>,
    bytes: usize,
}
static TX: OnceLock<Sender<Retired>> = OnceLock::new();

pub fn retire<T: Send + 'static>(value: T, estimated_bytes: usize) {
    let tx = TX.get_or_init(|| {
        let (tx, rx) = unbounded::<Retired>();
        std::thread::Builder::new()
            .name("resource-reclaimer".into())
            .spawn(move || {
                while let Ok(item) = rx.recv() {
                    drop(item.value);
                    BYTES.fetch_sub(item.bytes, Ordering::AcqRel);
                    QUEUED.fetch_sub(1, Ordering::AcqRel);
                }
            })
            .expect("resource reclaimer");
        tx
    });
    QUEUED.fetch_add(1, Ordering::AcqRel);
    BYTES.fetch_add(estimated_bytes, Ordering::AcqRel);
    if let Err(error) = tx.send(Retired {
        value: Box::new(value),
        bytes: estimated_bytes,
    }) {
        BYTES.fetch_sub(error.0.bytes, Ordering::AcqRel);
        QUEUED.fetch_sub(1, Ordering::AcqRel);
    }
}

pub fn record_metrics() {
    crate::performance::gauge("cpu_retired_count", QUEUED.load(Ordering::Acquire) as f64);
    crate::performance::gauge(
        "cpu_retired_estimated_bytes",
        BYTES.load(Ordering::Acquire) as f64,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn final_drop_occurs_on_reclaimer_thread() {
        struct Probe(std::sync::mpsc::Sender<String>);
        impl Drop for Probe {
            fn drop(&mut self) {
                let _ = self
                    .0
                    .send(std::thread::current().name().unwrap_or("").to_owned());
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        retire(Probe(tx), 1);
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),
            "resource-reclaimer"
        );
    }
}
