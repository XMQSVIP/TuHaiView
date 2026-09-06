use crate::{
    models::{ImageRecord, SortMode},
    performance,
    search::{self, SearchQuery},
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
    search: SearchQuery,
    records: Arc<[Arc<ImageRecord>]>,
    groups: Vec<(Arc<[ImageRecord]>, i64)>,
    // Shared published positions, never indices from an older catalog snapshot.
    stable_positions: Option<Arc<HashMap<i64, usize>>>,
}
pub struct SortResult {
    pub serial: u64,
    pub generation: u64,
    pub revision: u64,
    pub mode: SortMode,
    pub search: SearchQuery,
    pub hidden_duplicates: usize,
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
                'requests: while notify_rx.recv().is_ok() {
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
                    let mut indices = Vec::new();
                    let mut hidden_duplicates = 0;
                    for (index, record) in request.records.iter().enumerate() {
                        if index % 1024 == 0
                            && request.serial
                                != worker_serial.load(std::sync::atomic::Ordering::Acquire)
                        {
                            continue 'requests;
                        }
                        if hidden.contains(&record.id) {
                            hidden_duplicates += 1;
                        } else if search::matches(&record.file_name, &request.search.text) {
                            indices.push(index);
                        }
                    }
                    indices.sort_by(|a, b| {
                        let a = &request.records[*a];
                        let b = &request.records[*b];
                        if let Some(positions) = &request.stable_positions {
                            match (positions.get(&a.id), positions.get(&b.id)) {
                                (Some(a), Some(b)) => return a.cmp(b),
                                (Some(_), None) => return std::cmp::Ordering::Less,
                                (None, Some(_)) => return std::cmp::Ordering::Greater,
                                (None, None) => {}
                            }
                        }
                        compare(a, b, request.mode)
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
                        search: request.search,
                        hidden_duplicates,
                        indices: indices.into(),
                        positions: Arc::new(positions),
                    };
                    if let Ok(old) = discard_rx.try_recv() {
                        retire_order(old.indices, old.positions);
                    }
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
    #[cfg(test)]
    pub fn submit(
        &self,
        generation: u64,
        revision: u64,
        mode: SortMode,
        records: Arc<[Arc<ImageRecord>]>,
        groups: Vec<(Arc<[ImageRecord]>, i64)>,
        search: SearchQuery,
    ) {
        self.submit_with_stable_positions(
            generation, revision, mode, records, groups, search, None,
        );
    }

    pub fn submit_with_stable_positions(
        &self,
        generation: u64,
        revision: u64,
        mode: SortMode,
        records: Arc<[Arc<ImageRecord>]>,
        groups: Vec<(Arc<[ImageRecord]>, i64)>,
        search: SearchQuery,
        stable_positions: Option<Arc<HashMap<i64, usize>>>,
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
            search,
            records,
            groups,
            stable_positions,
        });
        if let Some(old) = old {
            crate::retirement::retire(old, std::mem::size_of::<SortRequest>());
        }
        let _ = self.notify_tx.try_send(());
    }
    pub fn is_current(&self, result: &SortResult) -> bool {
        result.serial == self.serial.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn cancel(&self) {
        self.serial
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if let Some(old) = self.pending.lock().take() {
            crate::retirement::retire(old, std::mem::size_of::<SortRequest>());
        }
        while let Ok(old) = self.rx.try_recv() {
            retire_order(old.indices, old.positions);
        }
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

    fn records(names: &[&str]) -> Arc<[Arc<ImageRecord>]> {
        names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                Arc::new(ImageRecord {
                    id: i as i64,
                    path: format!("folder{i}/{name}").into(),
                    relative_path: format!("folder{i}/{name}"),
                    file_name: (*name).into(),
                    size: (i + 1) as u64,
                    modified_ns: i as i64,
                    width: None,
                    height: None,
                    format: "jpg".into(),
                    thumbnail_key: format!("key{i}"),
                    content_hash: None,
                })
            })
            .collect::<Vec<_>>()
            .into()
    }

    fn receive(service: &SortService) -> SortResult {
        service
            .rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
    }

    #[test]
    fn scan_batches_preserve_published_ids_then_finish_in_requested_order() {
        let service = SortService::new(Arc::new(|| {}));
        // Every later record should precede the earlier ones in all four modes.
        let all: Arc<[Arc<ImageRecord>]> = (0..6)
            .map(|id| {
                let name = format!("image-{}.jpg", 6 - id);
                Arc::new(ImageRecord {
                    id,
                    path: name.clone().into(),
                    relative_path: name.clone(),
                    file_name: name,
                    size: id as u64,
                    modified_ns: id,
                    width: None,
                    height: None,
                    format: "jpg".into(),
                    thumbnail_key: format!("key{id}"),
                    content_hash: None,
                })
            })
            .collect::<Vec<_>>()
            .into();
        for mode in [
            SortMode::ModifiedDesc,
            SortMode::NameNatural,
            SortMode::SizeDesc,
            SortMode::Path,
        ] {
            let mut previous = Arc::new(HashMap::new());
            let mut visible = Vec::new();
            for (revision, count) in [(1, 2), (2, 4), (3, 6)] {
                // Reverse storage positions too, as catalog snapshots may insert paths anywhere.
                let snapshot: Arc<[Arc<ImageRecord>]> = all[..count]
                    .iter()
                    .rev()
                    .cloned()
                    .collect::<Vec<_>>()
                    .into();
                service.submit_with_stable_positions(
                    1,
                    revision,
                    mode,
                    snapshot.clone(),
                    vec![],
                    SearchQuery::default(),
                    Some(previous),
                );
                let result = receive(&service);
                let ids: Vec<_> = result.indices.iter().map(|i| snapshot[*i].id).collect();
                assert!(
                    ids.starts_with(&visible),
                    "scan displaced already displayed pictures"
                );
                for (position, id) in ids.iter().enumerate() {
                    assert_eq!(result.positions[id], position);
                }
                visible = ids;
                previous = result.positions;
            }
            assert_eq!(visible, [1, 0, 3, 2, 5, 4]);
            // Completion or an explicit sort ignores the temporary published order.
            service.submit(1, 3, mode, all.clone(), vec![], SearchQuery::default());
            let result = receive(&service);
            assert_eq!(&*result.indices, &[5, 4, 3, 2, 1, 0]);
        }
    }

    #[test]
    fn stable_scan_order_still_filters_deleted_renamed_and_hidden_records() {
        let service = SortService::new(Arc::new(|| {}));
        let original = records(&["cat0.jpg", "cat1.jpg", "cat2.jpg", "cat3.jpg", "cat4.jpg"]);
        // ID 0 removed; ID 1 renamed out of search; ID 2 hidden; ID 4 newly found.
        let mut renamed = original[1].as_ref().clone();
        renamed.file_name = "dog.jpg".into();
        let snapshot: Arc<[Arc<ImageRecord>]> = vec![
            original[4].clone(),
            original[3].clone(),
            Arc::new(renamed),
            original[2].clone(),
        ]
        .into();
        service.submit_with_stable_positions(
            1,
            2,
            SortMode::ModifiedDesc,
            snapshot.clone(),
            vec![(
                vec![original[2].as_ref().clone(), original[3].as_ref().clone()].into(),
                3,
            )],
            SearchQuery {
                text: "cat".into(),
                version: 1,
            },
            Some(Arc::new(HashMap::from([(0, 0), (1, 1), (2, 2), (3, 3)]))),
        );
        let result = receive(&service);
        assert_eq!(&*result.indices, &[1, 0]);
        assert_eq!(*result.positions, HashMap::from([(3, 0), (4, 1)]));
        assert_eq!(result.hidden_duplicates, 1);
        // No stale scan result may override the final ordering request.
        service.submit(
            1,
            2,
            SortMode::ModifiedDesc,
            snapshot,
            vec![],
            SearchQuery::default(),
        );
        assert!(!service.is_current(&result));
        assert_eq!(&*receive(&service).indices, &[0, 1, 3, 2]);
    }

    #[test]
    fn search_composes_with_all_sorts_and_keeps_original_duplicate_representative() {
        let records = records(&["旅行10.JPG", "旅行2.jpg", "其他.jpg", "旅行2.jpg"]);
        let service = SortService::new(Arc::new(|| {}));
        for (mode, expected) in [
            (SortMode::NameNatural, vec![1, 0]),
            (SortMode::ModifiedDesc, vec![1, 0]),
            (SortMode::SizeDesc, vec![1, 0]),
            (SortMode::Path, vec![0, 1]),
        ] {
            let query = SearchQuery {
                text: search::normalize(" 旅行 "),
                version: 1,
            };
            service.submit(
                1,
                1,
                mode,
                records.clone(),
                vec![(
                    vec![records[2].as_ref().clone(), records[3].as_ref().clone()].into(),
                    2,
                )],
                query.clone(),
            );
            let result = receive(&service);
            assert_eq!(&*result.indices, &expected);
            assert_eq!(
                result.hidden_duplicates, 1,
                "search exclusions are not duplicates"
            );
            assert_eq!(result.search, query);
            for (position, index) in result.indices.iter().enumerate() {
                assert_eq!(result.positions[&records[*index].id], position);
            }
        }
        service.submit(
            1,
            1,
            SortMode::NameNatural,
            records.clone(),
            vec![],
            SearchQuery {
                text: "旅行2".into(),
                version: 2,
            },
        );
        assert_eq!(
            &*receive(&service).indices,
            &[1, 3],
            "same names in different folders both match"
        );
        service.submit(
            1,
            1,
            SortMode::Path,
            records.clone(),
            vec![],
            SearchQuery {
                text: "folder".into(),
                version: 3,
            },
        );
        assert!(
            receive(&service).indices.is_empty(),
            "paths must not be searched"
        );
        service.submit(
            1,
            1,
            SortMode::Path,
            records.clone(),
            vec![(
                vec![records[2].as_ref().clone(), records[3].as_ref().clone()].into(),
                2,
            )],
            SearchQuery {
                text: "2.jpg".into(),
                version: 4,
            },
        );
        assert_eq!(
            &*receive(&service).indices,
            &[1],
            "matching hidden names do not replace keeper"
        );
    }

    #[test]
    fn latest_request_wins_across_queries_snapshots_and_directories() {
        let service = SortService::new(Arc::new(|| {}));
        let original = records(&["cat.jpg", "dog.jpg"]);
        service.submit(
            1,
            1,
            SortMode::Path,
            original.clone(),
            vec![],
            SearchQuery {
                text: "cat".into(),
                version: 1,
            },
        );
        let stale = receive(&service);
        for version in 2..50 {
            service.submit(
                1,
                1,
                SortMode::NameNatural,
                original.clone(),
                vec![],
                SearchQuery {
                    text: if version % 2 == 0 { "dog" } else { "cat" }.into(),
                    version,
                },
            );
        }
        // Simulate a rename, removal and addition, with different record positions.
        let changed = records(&["dog-renamed.jpg", "new-cat.jpg", "cat.jpg"]);
        service.submit(
            1,
            2,
            SortMode::Path,
            changed,
            vec![],
            SearchQuery {
                text: "cat".into(),
                version: 50,
            },
        );
        // Switching root must discard even a matching search/version from the former root.
        let new_root = records(&["new.jpg"]);
        service.submit(
            2,
            1,
            SortMode::Path,
            new_root,
            vec![],
            SearchQuery::default(),
        );
        assert!(!service.is_current(&stale));
        loop {
            let result = receive(&service);
            if !service.is_current(&result) {
                continue;
            }
            assert_eq!((result.generation, result.revision), (2, 1));
            assert_eq!(result.search, SearchQuery::default());
            assert_eq!(&*result.indices, &[0]);
            assert_eq!(result.positions.len(), 1);
            break;
        }
    }

    #[test]
    fn refreshed_snapshot_keeps_query_and_updated_indices() {
        let service = SortService::new(Arc::new(|| {}));
        let query = SearchQuery {
            text: "cat".into(),
            version: 8,
        };
        service.submit(
            3,
            1,
            SortMode::Path,
            records(&["cat.jpg", "dog.jpg"]),
            vec![],
            query.clone(),
        );
        assert_eq!(&*receive(&service).indices, &[0]);
        service.submit(
            3,
            2,
            SortMode::Path,
            records(&["renamed.jpg", "cat2.jpg", "cat3.jpg"]),
            vec![],
            query.clone(),
        );
        let result = receive(&service);
        assert_eq!(&*result.indices, &[1, 2]);
        assert_eq!(result.search, query);
        assert_eq!(result.positions, Arc::new(HashMap::from([(1, 0), (2, 1)])));
        service.submit(
            4,
            2,
            SortMode::Path,
            records(&["cat2.jpg"]),
            vec![],
            query.clone(),
        );
        let result = receive(&service);
        assert_eq!(result.search, query);
        assert_eq!(&*result.indices, &[0]);
    }

    #[test]
    #[ignore = "release-only 50k filename search latency benchmark; no disk images or GPU"]
    fn search_50k_latency() {
        assert!(!cfg!(debug_assertions), "run this benchmark with --release");
        let names: Vec<_> = (0..50_000)
            .rev()
            .map(|i| format!("旅行 IMG {i:05}.JPG"))
            .collect();
        let names: Vec<_> = names.iter().map(String::as_str).collect();
        let records = records(&names);
        let service = SortService::new(Arc::new(|| {}));
        for (label, text, count) in [
            ("many", "旅行", 50_000),
            ("few", "49999", 1),
            ("none", "missing", 0),
        ] {
            let mut times = Vec::new();
            for version in 1..=20 {
                let started = Instant::now();
                std::thread::sleep(crate::search::SEARCH_DELAY);
                let query = SearchQuery {
                    text: text.into(),
                    version,
                };
                service.submit(
                    1,
                    1,
                    SortMode::NameNatural,
                    records.clone(),
                    vec![],
                    query.clone(),
                );
                let result = receive(&service);
                assert_eq!(result.indices.len(), count);
                assert_eq!(result.search, query);
                times.push(started.elapsed().as_secs_f64() * 1000.0);
                retire_order(result.indices, result.positions);
            }
            times.sort_by(f64::total_cmp);
            let p95 = times[18];
            println!(
                "search_50k {label}: n=20, debounce+worker_receive p50={:.2}ms p95={p95:.2}ms max={:.2}ms (not screen presentation)",
                times[9], times[19]
            );
            assert!(p95 <= 300.0, "{label} exceeds the 300ms target: {p95:.2}ms");
        }
    }

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
            SearchQuery::default(),
        );
        let result = service
            .rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(&*result.indices, &[1, 0]);
        assert_eq!(result.positions[&0], 1);
        assert_eq!(records[0].file_name, "image10.jpg");
        assert_eq!(records[0].width, Some(4000));
        service.submit(
            7,
            3,
            SortMode::NameNatural,
            records,
            Vec::new(),
            SearchQuery::default(),
        );
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
