use anyhow::{Context, Result};
use std::{env, fs, path::PathBuf};

/// 便携版数据目录：始终位于正在运行的 exe 同级目录。
///
/// 索引和缩略图不会悄悄写入通常位于 C 盘的 `%LOCALAPPDATA%`，便于移动程序和控制磁盘占用。
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
