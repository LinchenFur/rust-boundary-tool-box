//! 通用时间、目录和隐藏进程工具。

use super::*;

use std::ffi::OsStr;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Result;
use chrono::Local;

/// 供 UI 和日志使用的本地时间戳。
pub fn now_text() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 写入元数据的紧凑 ISO 风格时间戳。
pub fn iso_now() -> String {
    Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

/// 将安装器数据放在可执行文件旁边或 installer_tool 目录下。
pub(crate) fn resolve_installer_home(runtime_dir: &Path) -> PathBuf {
    if runtime_dir
        .file_name()
        .map(|name| {
            name.to_string_lossy()
                .eq_ignore_ascii_case(INSTALLER_FOLDER_NAME)
        })
        .unwrap_or(false)
    {
        runtime_dir.to_path_buf()
    } else {
        runtime_dir.join(INSTALLER_FOLDER_NAME)
    }
}

/// 创建目录树，并把文件系统错误转成 anyhow。
pub(crate) fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    Ok(())
}

/// 创建无控制台窗口且不继承标准输入输出的 Windows 命令。
pub(crate) fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// 单独封装 taskkill，便于统一参数和后续测试。
pub(crate) fn hidden_taskkill_command() -> Command {
    hidden_command("taskkill")
}
