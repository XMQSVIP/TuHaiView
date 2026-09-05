use crate::models::{ConflictPolicy, FileAction, FileOperationReport, FileOperationRequest};
use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, bounded};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    thread,
};

pub struct FileOperationService {
    tx: Sender<FileOperationRequest>,
    pub rx: Receiver<(FileAction, FileOperationReport)>,
}

impl FileOperationService {
    pub fn new(wakeup: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (tx, work_rx) = bounded::<FileOperationRequest>(8);
        let (result_tx, rx) = bounded(8);
        thread::Builder::new()
            .name("windows-file-operations".into())
            .spawn(move || {
                #[cfg(windows)]
                unsafe {
                    // IFileOperation 依赖 STA COM；所有 Shell 文件操作集中在此线程，
                    // 既支持回收站语义，也不会阻塞 egui 的 UI 线程。
                    use windows::Win32::System::Com::{
                        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
                    };
                    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
                }
                while let Ok(request) = work_rx.recv() {
                    let action = request.action;
                    let report = execute(request).unwrap_or_else(|error| FileOperationReport {
                        failed: vec![(PathBuf::new(), error.to_string())],
                        ..Default::default()
                    });
                    let _ = result_tx.send((action, report));
                    wakeup();
                }
                #[cfg(windows)]
                unsafe {
                    windows::Win32::System::Com::CoUninitialize();
                }
            })
            .expect("failed to create file operation thread");
        Self { tx, rx }
    }

    pub fn submit(&self, request: FileOperationRequest) -> Result<()> {
        self.tx.try_send(request).context("文件操作队列正忙")
    }
}

pub fn execute(request: FileOperationRequest) -> Result<FileOperationReport> {
    match request.action {
        FileAction::PermanentDelete => permanent_delete(request.sources),
        _ => {
            #[cfg(windows)]
            {
                execute_shell(request)
            }
            #[cfg(not(windows))]
            {
                anyhow::bail!("此文件操作仅支持 Windows")
            }
        }
    }
}

fn permanent_delete(sources: Vec<PathBuf>) -> Result<FileOperationReport> {
    let mut report = FileOperationReport::default();
    for source in sources {
        // 永久删除不会走 Shell/回收站；调用方必须已完成不可恢复确认。
        let result = if source.is_dir() {
            fs::remove_dir(&source)
        } else {
            fs::remove_file(&source)
        };
        match result {
            Ok(()) => report.succeeded.push(source),
            Err(error) => report.failed.push((source, error.to_string())),
        }
    }
    Ok(report)
}

#[cfg(windows)]
fn execute_shell(request: FileOperationRequest) -> Result<FileOperationReport> {
    use windows::{
        Win32::{
            System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
            UI::Shell::{
                FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOCONFIRMMKDIR, FOF_RENAMEONCOLLISION,
                FOFX_ADDUNDORECORD, FOFX_RECYCLEONDELETE, FOFX_SHOWELEVATIONPROMPT, FileOperation,
                IFileOperation, IShellItem,
            },
        },
        core::{HSTRING, PCWSTR},
    };

    unsafe {
        let operation: IFileOperation =
            CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
        let mut flags = FOF_NOCONFIRMATION | FOF_NOCONFIRMMKDIR | FOFX_SHOWELEVATIONPROMPT;
        if request.action == FileAction::RecycleDelete {
            // 同时设置 UNDO 和 RECYCLE 标志，确保普通 Delete 默认进入回收站。
            flags |= FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE | FOFX_ADDUNDORECORD;
        }
        if request.conflict == ConflictPolicy::AutoRename {
            flags |= FOF_RENAMEONCOLLISION;
        }
        operation.SetOperationFlags(flags)?;

        let destination_item: Option<IShellItem> = request
            .destination
            .as_ref()
            .map(|path| shell_item(path))
            .transpose()?;
        let mut planned = Vec::<(PathBuf, Option<PathBuf>)>::new();
        let mut report = FileOperationReport::default();
        // reserved 防止同一批来源文件在“扁平复制到目标目录”时彼此撞名。
        let mut reserved = HashSet::<String>::new();

        for source in request.sources {
            if !source.exists() {
                report.skipped.push((source, "源文件不存在".into()));
                continue;
            }
            let source_item = match shell_item(&source) {
                Ok(item) => item,
                Err(error) => {
                    report.failed.push((source, error.to_string()));
                    continue;
                }
            };
            match request.action {
                FileAction::RecycleDelete => {
                    if let Err(error) = operation.DeleteItem(&source_item, None) {
                        report.failed.push((source, error.to_string()));
                    } else {
                        planned.push((source, None));
                    }
                }
                FileAction::Copy | FileAction::Move => {
                    let destination = request.destination.as_ref().context("缺少目标文件夹")?;
                    let destination_item = destination_item.as_ref().context("目标文件夹无效")?;
                    let policy = request
                        .conflict_overrides
                        .get(&source)
                        .copied()
                        .unwrap_or(request.conflict);
                    let target = resolve_destination(destination, &source, policy, &mut reserved)?;
                    let Some(target) = target else {
                        report.skipped.push((source, "同名文件已跳过".into()));
                        continue;
                    };
                    if request.action == FileAction::Move && same_path(&source, &target) {
                        report.skipped.push((source, "源位置和目标位置相同".into()));
                        continue;
                    }
                    let name = HSTRING::from(
                        target
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .as_ref(),
                    );
                    let result = if request.action == FileAction::Copy {
                        operation.CopyItem(
                            &source_item,
                            destination_item,
                            PCWSTR(name.as_ptr()),
                            None,
                        )
                    } else {
                        operation.MoveItem(
                            &source_item,
                            destination_item,
                            PCWSTR(name.as_ptr()),
                            None,
                        )
                    };
                    if let Err(error) = result {
                        report.failed.push((source, error.to_string()));
                    } else {
                        planned.push((source, Some(target)));
                    }
                }
                FileAction::PermanentDelete => unreachable!(),
            }
        }

        if planned.is_empty() {
            return Ok(report);
        }
        if let Err(error) = operation.PerformOperations() {
            for (source, _) in planned {
                report.failed.push((source, error.to_string()));
            }
            return Ok(report);
        }
        // Shell API 可能只完成一部分，因此逐项通过源/目标的最终状态核验结果。
        report.cancelled = operation.GetAnyOperationsAborted()?.as_bool();
        for (source, target) in planned {
            let success = match request.action {
                FileAction::RecycleDelete => !source.exists(),
                FileAction::Copy => target.as_ref().is_some_and(|path| path.exists()),
                FileAction::Move => {
                    !source.exists() && target.as_ref().is_some_and(|path| path.exists())
                }
                FileAction::PermanentDelete => false,
            };
            if success {
                report.succeeded.push(source);
            } else if report.cancelled {
                report.skipped.push((source, "用户取消".into()));
            } else {
                report.failed.push((source, "Windows 未完成该操作".into()));
            }
        }
        Ok(report)
    }
}

#[cfg(windows)]
fn shell_item(path: &Path) -> Result<windows::Win32::UI::Shell::IShellItem> {
    use windows::{Win32::UI::Shell::SHCreateItemFromParsingName, core::HSTRING};
    let text = HSTRING::from(path.as_os_str());
    unsafe { SHCreateItemFromParsingName(&text, None).map_err(Into::into) }
}

fn resolve_destination(
    dir: &Path,
    source: &Path,
    policy: ConflictPolicy,
    reserved: &mut HashSet<String>,
) -> Result<Option<PathBuf>> {
    let name = source.file_name().context("无效文件名")?;
    let mut destination = dir.join(name);
    let mut collision =
        destination.exists() || reserved.contains(&destination.to_string_lossy().to_lowercase());
    if !collision {
        reserved.insert(destination.to_string_lossy().to_lowercase());
        return Ok(Some(destination));
    }
    match policy {
        ConflictPolicy::Ask | ConflictPolicy::Skip => Ok(None),
        ConflictPolicy::Overwrite => {
            reserved.insert(destination.to_string_lossy().to_lowercase());
            Ok(Some(destination))
        }
        ConflictPolicy::AutoRename => {
            let stem = source
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("file");
            let extension = source
                .extension()
                .and_then(|value| value.to_str())
                .map(|value| format!(".{value}"))
                .unwrap_or_default();
            for index in 1..100_000 {
                destination = dir.join(format!("{stem} ({index}){extension}"));
                collision = destination.exists()
                    || reserved.contains(&destination.to_string_lossy().to_lowercase());
                if !collision {
                    reserved.insert(destination.to_string_lossy().to_lowercase());
                    return Ok(Some(destination));
                }
            }
            anyhow::bail!("无法生成不冲突的文件名")
        }
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .eq_ignore_ascii_case(right.to_string_lossy().trim_end_matches(['\\', '/']))
}

pub fn reveal_in_explorer(path: &Path) -> Result<()> {
    Command::new("explorer.exe")
        .arg(format!("/select,{}", path.display()))
        .spawn()?;
    Ok(())
}

pub fn open_with_default(path: &Path) -> Result<()> {
    open::that(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn same_windows_path_is_case_insensitive() {
        assert!(same_path(
            Path::new(r"C:\Temp\a.jpg"),
            Path::new(r"c:\temp\A.JPG")
        ));
    }
}
