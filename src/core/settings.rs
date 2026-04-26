//! InstallerCore 置顶配置读写。

use std::fs;
use std::path::Path;

use anyhow::Result;

use super::util::{ensure_dir, normalize_hotkey, normalize_keep_topmost, normalize_topmost_mode};
use super::*;

impl InstallerCore {
    /// 读取持久化的置顶设置；缺失时使用安全默认值。
    pub fn read_topmost_config(&self, target_win64: &Path) -> TopmostConfig {
        let paths = self.metadata_paths(target_win64);
        let mode = fs::read_to_string(paths.topmost_mode_file)
            .ok()
            .map(|value| normalize_topmost_mode(value.trim()))
            .unwrap_or_else(|| DEFAULT_TOPMOST_MODE.to_string());
        let keep = fs::read_to_string(paths.topmost_keep_file)
            .ok()
            .map(|value| normalize_keep_topmost(value.trim()))
            .unwrap_or(DEFAULT_KEEP_TOPMOST);
        let hotkey = fs::read_to_string(paths.topmost_hotkey_file)
            .ok()
            .and_then(|value| normalize_hotkey(value.trim()).ok())
            .unwrap_or_else(|| DEFAULT_TOPMOST_HOTKEY.to_string());
        TopmostConfig {
            mode,
            keep_topmost: keep,
            hotkey,
        }
    }

    /// 规范化当前置顶设置并写入元数据。
    pub fn write_topmost_config(
        &self,
        target_win64: &Path,
        keep_topmost: bool,
        hotkey: &str,
    ) -> Result<TopmostConfig> {
        let paths = self.metadata_paths(target_win64);
        ensure_dir(&paths.metadata_dir)?;
        let config = TopmostConfig {
            mode: DEFAULT_TOPMOST_MODE.to_string(),
            keep_topmost: normalize_keep_topmost(keep_topmost),
            hotkey: normalize_hotkey(hotkey)?,
        };
        fs::write(&paths.topmost_mode_file, format!("{}\n", config.mode))?;
        fs::write(
            &paths.topmost_keep_file,
            if config.keep_topmost { "1\n" } else { "0\n" },
        )?;
        fs::write(&paths.topmost_hotkey_file, format!("{}\n", config.hotkey))?;
        self.log(format!(
            "启动置顶配置已写入：{} -> {}, {} -> {}, {} -> {}",
            paths.topmost_mode_file.display(),
            config.mode,
            paths.topmost_keep_file.display(),
            if config.keep_topmost { "1" } else { "0" },
            paths.topmost_hotkey_file.display(),
            config.hotkey
        ));
        Ok(config)
    }
}
