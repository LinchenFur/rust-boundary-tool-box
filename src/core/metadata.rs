//! InstallerCore 安装元数据路径、备份和读写。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::filesystem::{copy_path, read_json_file, write_json_file};
use super::util::iso_now;
use super::*;

impl InstallerCore {
    /// 计算所选安装目录对应的元数据位置。
    pub(super) fn metadata_paths(&self, target_win64: &Path) -> MetadataPaths {
        let metadata_dir = target_win64.join(METADATA_DIR_NAME);
        MetadataPaths {
            state_file: metadata_dir.join(STATE_FILE_NAME),
            markers_file: metadata_dir.join(MARKERS_FILE_NAME),
            topmost_mode_file: metadata_dir.join(TOPMOST_MODE_FILE_NAME),
            topmost_keep_file: metadata_dir.join(TOPMOST_KEEP_FILE_NAME),
            topmost_hotkey_file: metadata_dir.join(TOPMOST_HOTKEY_FILE_NAME),
            backups_root: metadata_dir.join("backups"),
            metadata_dir,
        }
    }

    /// 将已有的非安装器条目复制到本次安装的备份目录。
    pub(super) fn backup_item(
        &self,
        source_path: &Path,
        backup_root: &Path,
        item_name: &str,
    ) -> Result<String> {
        let backup_relative = PathBuf::from("backups")
            .join(
                backup_root
                    .file_name()
                    .unwrap_or_else(|| OsStr::new("backup")),
            )
            .join(item_name);
        let backup_target = backup_root.join(item_name);
        self.log(format!(
            "备份现有内容：{} -> {}",
            source_path.display(),
            backup_target.display()
        ));
        copy_path(source_path, &backup_target)?;
        Ok(backup_relative.to_string_lossy().replace('\\', "/"))
    }

    /// 写入标记文件；state.json 缺失时用于受限清理。
    pub(super) fn write_markers(&self, target_win64: &Path, install_id: String) -> Result<()> {
        let paths = self.metadata_paths(target_win64);
        let data = InstallMarkers {
            version: APP_VERSION.to_string(),
            install_id,
            managed_names: MANAGED_ITEMS
                .iter()
                .map(|item| item.name.to_string())
                .collect(),
            target_dir: target_win64.display().to_string(),
            updated_at: iso_now(),
        };
        write_json_file(&paths.markers_file, &data)
    }

    /// 读取完整安装状态。文件缺失返回 Ok(None)，JSON 损坏视为错误。
    pub(super) fn load_state(&self, target_win64: &Path) -> Result<Option<InstallState>> {
        read_json_file(&self.metadata_paths(target_win64).state_file)
    }

    /// 读取最小安装标记。文件缺失返回 Ok(None)，JSON 损坏视为错误。
    pub(super) fn load_markers(&self, target_win64: &Path) -> Result<Option<InstallMarkers>> {
        read_json_file(&self.metadata_paths(target_win64).markers_file)
    }
}
