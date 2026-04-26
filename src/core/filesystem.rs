//! 文件系统与 JSON 元数据读写工具。

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

/// 读取可选 JSON 元数据；数据损坏时作为阻塞错误处理。
pub(crate) fn read_json_file<T>(path: &Path) -> Result<Option<T>>
where
    T: for<'de> Deserialize<'de>,
{
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path)
        .with_context(|| format!("读取 JSON 文件失败：{}", path.display()))?;
    let value = serde_json::from_str(&text)
        .with_context(|| format!("解析 JSON 文件失败：{}", path.display()))?;
    Ok(Some(value))
}

/// 写入格式化 JSON 元数据，并按需创建父目录。
pub(crate) fn write_json_file<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(value)?;
    fs::write(path, text)?;
    Ok(())
}

/// 删除存在的文件或目录。
pub(crate) fn delete_path(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// 复制单个文件或整个目录树。
pub(crate) fn copy_path(src: &Path, dst: &Path) -> Result<()> {
    if src.is_dir() {
        for entry in WalkDir::new(src) {
            let entry = entry?;
            let relative = entry.path().strip_prefix(src)?;
            let target = dst.join(relative);
            if entry.file_type().is_dir() {
                fs::create_dir_all(&target)?;
            } else {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(entry.path(), &target)?;
            }
        }
    } else {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dst)?;
    }
    Ok(())
}
