use crate::catalog::sort_records;
use crate::models::{ImageRecord, SortMode};
use crossbeam_channel::{Receiver, Sender, bounded};
use std::{sync::Arc, thread};

struct SortRequest {
    generation: u64,
    revision: u64,
    mode: SortMode,
    records: Vec<ImageRecord>,
}

pub struct SortResult {
    pub generation: u64,
    pub revision: u64,
    pub mode: SortMode,
    pub records: Vec<ImageRecord>,
}

pub struct SortService {
    pending: Arc<parking_lot::Mutex<Option<SortRequest>>>,
    notify_tx: Sender<()>,
    pub rx: Receiver<SortResult>,
}

impl SortService {
    pub fn new(wakeup: Arc<dyn Fn() + Send + Sync>) -> Self {
        let pending = Arc::new(parking_lot::Mutex::new(None::<SortRequest>));
        let (notify_tx, notify_rx) = bounded::<()>(1);
        let (result_tx, rx) = bounded::<SortResult>(2);
        let worker_pending = pending.clone();
        thread::Builder::new()
            .name("image-record-sorter".into())
            .spawn(move || {
                while notify_rx.recv().is_ok() {
                    let Some(mut request) = worker_pending.lock().take() else {
                        continue;
                    };
                    sort_records(&mut request.records, request.mode);
                    wakeup();
                    if result_tx
                        .send(SortResult {
                            generation: request.generation,
                            revision: request.revision,
                            mode: request.mode,
                            records: request.records,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("failed to create sort worker");
        Self {
            pending,
            notify_tx,
            rx,
        }
    }

    pub fn submit(
        &self,
        generation: u64,
        revision: u64,
        mode: SortMode,
        records: Vec<ImageRecord>,
    ) {
        *self.pending.lock() = Some(SortRequest {
            generation,
            revision,
            mode,
            records,
        });
        let _ = self.notify_tx.try_send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::PathBuf, time::Duration};

    fn record(name: &str) -> ImageRecord {
        ImageRecord {
            id: 0,
            path: PathBuf::from(name),
            relative_path: name.into(),
            file_name: name.into(),
            size: 0,
            modified_ns: 0,
            width: None,
            height: None,
            format: "jpg".into(),
            thumbnail_key: name.into(),
        }
    }

    #[test]
    fn worker_sorts_records_without_blocking_caller() {
        let service = SortService::new(Arc::new(|| {}));
        service.submit(
            7,
            3,
            SortMode::NameNatural,
            vec![record("image10.jpg"), record("image2.jpg")],
        );
        let result = service.rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(result.generation, 7);
        assert_eq!(result.revision, 3);
        assert_eq!(result.records[0].file_name, "image2.jpg");
    }
}
