use anyhow::{Context, Result};
use std::{env, fs, path::PathBuf};

/// Portable application data directory next to the running executable.
///
/// Keeping this directory beside the executable means the catalog and
/// thumbnails stay on the same drive as the portable program instead of
/// silently consuming space on `%LOCALAPPDATA%` (usually C: on Windows).
pub fn data_dir() -> Result<PathBuf> {
    let executable = env::current_exe().context("无法定位程序所在目录")?;
    let parent = executable.parent().context("无法定位程序所在目录")?;
    let directory = parent.join("data");
    fs::create_dir_all(&directory)?;
    Ok(directory)
}

pub fn database_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("catalog.sqlite3"))
}

pub fn thumbnail_cache_dir() -> Result<PathBuf> {
    let directory = data_dir()?.join("cache").join("thumbnails");
    fs::create_dir_all(&directory)?;
    Ok(directory)
}

pub fn clear_thumbnail_cache() -> Result<()> {
    let directory = thumbnail_cache_dir()?;
    if directory.exists() {
        fs::remove_dir_all(&directory)?;
    }
    fs::create_dir_all(&directory)?;
    Ok(())
}

/// Remove data written by versions that used Windows user profile folders.
/// The targets are fixed application-specific directories and never include
/// any user images.
pub fn clear_legacy_storage() -> Result<()> {
    let executable_parent = env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(PathBuf::from));
    let mut directories = Vec::new();
    if let Some(local) = env::var_os("LOCALAPPDATA") {
        directories.push(
            PathBuf::from(local)
                .join("Codex")
                .join("RustImagePreviewer"),
        );
    }
    if let Some(roaming) = env::var_os("APPDATA") {
        directories.push(
            PathBuf::from(roaming)
                .join("Codex")
                .join("RustImagePreviewer"),
        );
    }
    for directory in directories {
        // A user may have placed the portable executable in the old storage
        // directory. Never attempt to remove the directory containing the
        // running program itself.
        if executable_parent.as_ref() == Some(&directory) {
            continue;
        }
        match fs::remove_dir_all(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
