//! InstallerCore 游戏目录识别接口。

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::pathing::{
    detect_steam_game_win64, list_available_drives, normalize_selected_path, scan_drives_for_game,
};
use super::*;

impl InstallerCore {
    /// 接受游戏根目录、ProjectBoundary 目录或 Binaries\Win64 目录。
    pub fn normalize_selected_path(&self, raw: impl AsRef<Path>) -> Result<PathBuf> {
        normalize_selected_path(raw.as_ref())
    }

    /// 通过 Steam 注册表和库元数据查找 Boundary。
    pub fn detect_steam_game_win64(&self) -> Result<(PathBuf, String)> {
        detect_steam_game_win64()
    }

    /// 列出扫描弹窗可用的 Windows 盘符根目录。
    pub fn list_available_drives(&self) -> Vec<PathBuf> {
        list_available_drives()
    }

    /// 并行扫描所选盘符，并返回第一个有效 Win64 目录。
    pub fn scan_drives_for_game(&self, drives: &[PathBuf]) -> Option<PathBuf> {
        scan_drives_for_game(drives, self.logger.clone())
    }
}
