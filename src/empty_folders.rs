use crate::models::EmptyFolderCandidate;
use crossbeam_channel::{Receiver, Sender, unbounded};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub enum EmptyFolderEvent {
    Progress {
        generation: u64,
        visited_directories: usize,
        found: usize,
        errors: usize,
    },
    Finished {
        generation: u64,
        folders: Vec<EmptyFolderCandidate>,
        visited_directories: usize,
        errors: usize,
        elapsed_ms: u128,
    },
    Error {
        generation: u64,
        message: String,
    },
}

pub struct EmptyFolderService {
    tx: Sender<EmptyFolderEvent>,
    pub rx: Receiver<EmptyFolderEvent>,
    generation: u64,
    cancel: Option<Arc<AtomicBool>>,
    wakeup: Arc<dyn Fn() + Send + Sync>,
}

impl EmptyFolderService {
    pub fn new(wakeup: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (tx, rx) = unbounded();
        Self {
            tx,
            rx,
            generation: 0,
            cancel: None,
            wakeup,
        }
    }

    pub fn scan(&mut self, root: PathBuf) -> u64 {
        self.cancel_token();
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        let tx = self.tx.clone();
        let wakeup = self.wakeup.clone();
        thread::Builder::new()
            .name("empty-folder-scanner".into())
            .spawn(move || {
                if let Err(message) = scan_worker(&root, generation, &cancel, &tx, &wakeup) {
                    if !cancel.load(Ordering::Acquire) {
                        let _ = tx.send(EmptyFolderEvent::Error {
                            generation,
                            message,
                        });
                        wakeup();
                    }
                }
            })
            .expect("failed to create empty folder scanner");
        generation
    }

    pub fn cancel(&mut self) -> u64 {
        self.cancel_token();
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    fn cancel_token(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Release);
        }
    }
}

impl Drop for EmptyFolderService {
    fn drop(&mut self) {
        self.cancel_token();
    }
}

fn scan_worker(
    root: &Path,
    generation: u64,
    cancel: &AtomicBool,
    tx: &Sender<EmptyFolderEvent>,
    wakeup: &Arc<dyn Fn() + Send + Sync>,
) -> Result<(), String> {
    if !root.is_dir() {
        return Err("根目录不存在或无法访问".into());
    }

    let started = Instant::now();
    let mut stack = vec![root.to_path_buf()];
    let mut folders = Vec::new();
    let mut visited_directories = 0_usize;
    let mut errors = 0_usize;
    let mut last_progress = Instant::now();

    while let Some(directory) = stack.pop() {
        if cancel.load(Ordering::Acquire) {
            return Ok(());
        }
        visited_directories += 1;
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if directory == root => {
                return Err(format!("无法读取根目录：{error}"));
            }
            Err(_) => {
                errors += 1;
                continue;
            }
        };

        let mut has_content = false;
        for entry in entries {
            if cancel.load(Ordering::Acquire) {
                return Ok(());
            }
            has_content = true;
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    errors += 1;
                    continue;
                }
            };
            match entry.file_type() {
                Ok(file_type) if file_type.is_dir() && !file_type.is_symlink() => {
                    stack.push(entry.path());
                }
                Ok(_) => {}
                Err(_) => errors += 1,
            }
        }

        if directory != root && !has_content {
            folders.push(EmptyFolderCandidate {
                relative_path: directory
                    .strip_prefix(root)
                    .unwrap_or(&directory)
                    .to_string_lossy()
                    .into_owned(),
                path: directory,
            });
        }

        if last_progress.elapsed() >= Duration::from_millis(120) {
            let _ = tx.send(EmptyFolderEvent::Progress {
                generation,
                visited_directories,
                found: folders.len(),
                errors,
            });
            wakeup();
            last_progress = Instant::now();
        }

        // Empty-folder discovery is an auxiliary operation. Yield regularly
        // so image enumeration and thumbnail decoding keep making progress on
        // slower disks while both scans are active.
        if visited_directories.is_multiple_of(64) {
            thread::sleep(Duration::from_millis(1));
        }
    }

    if cancel.load(Ordering::Acquire) {
        return Ok(());
    }
    folders.sort_by(|a, b| {
        a.relative_path
            .to_lowercase()
            .cmp(&b.relative_path.to_lowercase())
    });
    let _ = tx.send(EmptyFolderEvent::Finished {
        generation,
        folders,
        visited_directories,
        errors,
        elapsed_ms: started.elapsed().as_millis(),
    });
    wakeup();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn finds_only_directly_empty_subdirectories() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("tuhai-view-empty-{suffix}"));
        fs::create_dir_all(root.join("empty")).unwrap();
        fs::create_dir_all(root.join("parent-with-empty-child").join("child")).unwrap();
        fs::create_dir_all(root.join("contains-file")).unwrap();
        fs::write(
            root.join("contains-file").join("hidden-content.txt"),
            b"content",
        )
        .unwrap();

        let (tx, rx) = unbounded();
        let wakeup: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        scan_worker(&root, 1, &AtomicBool::new(false), &tx, &wakeup).unwrap();
        let folders = rx
            .try_iter()
            .find_map(|event| match event {
                EmptyFolderEvent::Finished { folders, .. } => Some(folders),
                _ => None,
            })
            .unwrap();
        let relative = folders
            .iter()
            .map(|folder| folder.relative_path.as_str())
            .collect::<Vec<_>>();
        assert!(relative.contains(&"empty"));
        assert!(relative.contains(&"parent-with-empty-child\\child"));
        assert!(!relative.contains(&"parent-with-empty-child"));
        assert!(!relative.contains(&"contains-file"));

        fs::remove_dir_all(root).unwrap();
    }
}
