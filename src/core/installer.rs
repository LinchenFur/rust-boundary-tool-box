//! InstallerCore 基础构造、日志和载荷说明。

use std::env;
use std::path::Path;

use anyhow::{Context, Result};

use super::util::{ensure_dir, now_text, resolve_installer_home};
use super::*;

impl InstallerCore {
    /// 创建核心服务，并准备工具箱主目录和日志目录。
    pub fn new(logger: Logger) -> Result<Self> {
        let current_exe = env::current_exe().context("无法定位当前程序")?;
        let runtime_dir = current_exe
            .parent()
            .map(Path::to_path_buf)
            .context("无法解析运行目录")?;
        let installer_home = resolve_installer_home(&runtime_dir);
        ensure_dir(&installer_home)?;
        ensure_dir(&installer_home.join("logs"))?;
        Ok(Self {
            runtime_dir,
            installer_home,
            logger,
        })
    }

    /// 通过 UI/会话 logger 发送带时间戳的日志行。
    pub fn log(&self, message: impl Into<String>) {
        (self.logger)(format!("[{}] {}", now_text(), message.into()));
    }

    /// 日志和设置页展示的载荷来源说明。
    pub fn payload_label(&self) -> &'static str {
        "内嵌载荷 + ProjectRebound 在线 Nightly"
    }
}
