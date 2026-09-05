use crate::{
    models::{ImageRecord, SortMode},
    performance,
};
use crossbeam_channel::{Receiver, Sender, bounded};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    thread,
    time::Instant,
};
struct SortRequest {
    generation: u64,
    revision: u64,
    serial: u64,
    mode: SortMode,
    records: Arc<[Arc<ImageRecord>]>,
    groups: Vec<(Arc<[ImageRecord]>, i64)>,
}
pub struct SortResult {
    pub serial: u64,
    pub generation: u64,
    pub revision: u64,
    pub mode: SortMode,
    pub indices: Arc<[usize]>,
    pub positions: Arc<HashMap<i64, usize>>,
}
pub fn retire_order(indices: Arc<[usize]>, positions: Arc<HashMap<i64, usize>>) {
    let bytes = indices.len() * std::mem::size_of::<usize>()
        + positions.len() * std::mem::size_of::<(i64, usize)>();
    crate::retirement::retire((indices, positions), bytes);
}
pub struct SortService {
    pending: Arc<parking_lot::Mutex<Option<SortRequest>>>,
    serial: Arc<std::sync::atomic::AtomicU64>,
    notify_tx: Sender<()>,
    pub rx: Receiver<SortResult>,
}
impl SortService {
    pub fn new(wakeup: Arc<dyn Fn() + Send + Sync>) -> Self {
        let pending = Arc::new(parking_lot::Mutex::new(None::<SortRequest>));
        let serial = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (notify_tx, notify_rx) = bounded::<()>(1);
        let (result_tx, rx) = bounded::<SortResult>(1);
        let discard_rx = rx.clone();
        let worker_pending = pending.clone();
        let worker_serial = serial.clone();
        thread::Builder::new()
            .name("image-index-sorter".into())
            .spawn(move || {
                while notify_rx.recv().is_ok() {
                    let Some(request) = worker_pending.lock().take() else {
                        continue;
                    };
                    let started = Instant::now();
                    let hidden: HashSet<i64> = request
                        .groups
                        .iter()
                        .flat_map(|(members, keeper)| {
                            members
                                .iter()
                                .filter(move |r| r.id != *keeper)
                                .map(|r| r.id)
                        })
                        .collect();
                    let mut indices: Vec<_> = (0..request.records.len())
                        .filter(|i| !hidden.contains(&request.records[*i].id))
                        .collect();
                    indices.sort_by(|a, b| {
                        compare(&request.records[*a], &request.records[*b], request.mode)
                    });
                    if request.serial != worker_serial.load(std::sync::atomic::Ordering::Acquire) {
                        continue;
                    }
                    let positions = indices
                        .iter()
                        .enumerate()
                        .map(|(display, index)| (request.records[*index].id, display))
                        .collect();
                    let result = SortResult {
                        serial: request.serial,
                        generation: request.generation,
                        revision: request.revision,
                        mode: request.mode,
                        indices: indices.into(),
                        positions: Arc::new(positions),
                    };
                    let _ = discard_rx.try_recv();
                    if result_tx.send(result).is_err() {
                        break;
                    }
                    wakeup();
                    performance::elapsed("sort_ms", started);
                }
            })
            .expect("failed to create index sorter");
        Self {
            pending,
            serial,
            notify_tx,
            rx,
        }
    }
    pub fn submit(
        &self,
        generation: u64,
        revision: u64,
        mode: SortMode,
        records: Arc<[Arc<ImageRecord>]>,
        groups: Vec<(Arc<[ImageRecord]>, i64)>,
    ) {
        let serial = self
            .serial
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
            .wrapping_add(1);
        let old = self.pending.lock().replace(SortRequest {
            generation,
            revision,
            serial,
            mode,
            records,
            groups,
        });
        if let Some(old) = old {
            crate::retirement::retire(old, std::mem::size_of::<SortRequest>());
        }
        let _ = self.notify_tx.try_send(());
    }
    pub fn is_current(&self, result: &SortResult) -> bool {
        result.serial == self.serial.load(std::sync::atomic::Ordering::Acquire)
    }
}
fn compare(a: &ImageRecord, b: &ImageRecord, mode: SortMode) -> std::cmp::Ordering {
    match mode {
        SortMode::ModifiedDesc => b.modified_ns.cmp(&a.modified_ns),
        SortMode::NameNatural => natord::compare_ignore_case(&a.file_name, &b.file_name),
        SortMode::SizeDesc => b.size.cmp(&a.size),
        SortMode::Path => natord::compare_ignore_case(&a.relative_path, &b.relative_path),
    }
    .then_with(|| a.id.cmp(&b.id))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sorts_indices_preserving_records_and_filtering_hidden_ids() {
        let records: Arc<[Arc<ImageRecord>]> = ["image10.jpg", "image2.jpg", "image1.jpg"]
            .into_iter()
            .enumerate()
            .map(|(i, name)| {
                Arc::new(ImageRecord {
                    id: i as i64,
                    path: name.into(),
                    relative_path: name.into(),
                    file_name: name.into(),
                    size: 1,
                    modified_ns: 0,
                    width: Some(4000),
                    height: Some(3000),
                    format: "jpg".into(),
                    thumbnail_key: name.into(),
                    content_hash: None,
                })
            })
            .collect::<Vec<_>>()
            .into();
        let service = SortService::new(Arc::new(|| {}));
        service.submit(
            7,
            3,
            SortMode::NameNatural,
            records.clone(),
            vec![(vec![records[2].as_ref().clone()].into(), -1)],
        );
        let result = service
            .rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(&*result.indices, &[1, 0]);
        assert_eq!(result.positions[&0], 1);
        assert_eq!(records[0].file_name, "image10.jpg");
        assert_eq!(records[0].width, Some(4000));
        service.submit(7, 3, SortMode::NameNatural, records, Vec::new());
        assert!(
            !service.is_current(&result),
            "published old filter must be rejected"
        );
        let updated = service
            .rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(service.is_current(&updated));
        assert_eq!(&*updated.indices, &[2, 1, 0]);
    }
}
