use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct CatalogSnapshot {
    pub generation: u64,
    /// Changes only for membership/file versions, not progressive metadata.
    pub revision: u64,
    pub records: Arc<[Arc<ImageRecord>]>,
    pub by_path: Arc<HashMap<PathBuf, usize>>,
    pub by_id: Arc<HashMap<i64, usize>>,
    pub natural_indices: Arc<[usize]>,
}

impl CatalogSnapshot {
    pub fn new(generation: u64, revision: u64, records: Vec<Arc<ImageRecord>>) -> Self {
        let by_path = records
            .iter()
            .enumerate()
            .map(|(i, r)| (r.path.clone(), i))
            .collect();
        let by_id = records.iter().enumerate().map(|(i, r)| (r.id, i)).collect();
        let natural_indices = (0..records.len()).collect::<Vec<_>>().into();
        Self {
            generation,
            revision,
            records: records.into(),
            by_path: Arc::new(by_path),
            by_id: Arc::new(by_id),
            natural_indices,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ImageRecord {
    pub id: i64,
    pub path: PathBuf,
    pub relative_path: String,
    pub file_name: String,
    pub size: u64,
    pub modified_ns: i64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub format: String,
    pub thumbnail_key: String,
    /// 文件内容的 SHA-256；只有大小和修改时间未变化时才可复用。
    pub content_hash: Option<String>,
}

#[derive(Clone, Debug)]
pub struct EmptyFolderCandidate {
    pub path: PathBuf,
    pub relative_path: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortMode {
    ModifiedDesc,
    NameNatural,
    SizeDesc,
    Path,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileAction {
    Copy,
    Move,
    RecycleDelete,
    PermanentDelete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictPolicy {
    Ask,
    Overwrite,
    Skip,
    AutoRename,
}

#[derive(Clone, Debug)]
pub struct FileOperationRequest {
    pub action: FileAction,
    pub sources: Vec<PathBuf>,
    pub destination: Option<PathBuf>,
    pub conflict: ConflictPolicy,
    pub conflict_overrides: HashMap<PathBuf, ConflictPolicy>,
    pub duplicate_check: Option<DuplicateDeleteCheck>,
}

#[derive(Clone, Debug)]
pub struct DuplicateDeleteCheck {
    pub snapshot: Arc<CatalogSnapshot>,
    /// Members, expected hash and optional ID to keep.
    pub groups: Vec<(Arc<[ImageRecord]>, String, Option<i64>)>,
}

#[derive(Clone, Debug, Default)]
pub struct FileOperationReport {
    pub succeeded: Vec<PathBuf>,
    pub affected_paths: Vec<PathBuf>,
    pub skipped: Vec<(PathBuf, String)>,
    pub failed: Vec<(PathBuf, String)>,
    pub cancelled: bool,
}

impl AsRef<ImageRecord> for ImageRecord {
    fn as_ref(&self) -> &ImageRecord {
        self
    }
}
