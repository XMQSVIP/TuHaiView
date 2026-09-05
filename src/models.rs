use std::collections::HashMap;
use std::path::PathBuf;

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
}

#[derive(Clone, Debug, Default)]
pub struct FileOperationReport {
    pub succeeded: Vec<PathBuf>,
    pub skipped: Vec<(PathBuf, String)>,
    pub failed: Vec<(PathBuf, String)>,
    pub cancelled: bool,
}
