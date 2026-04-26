//! InstallerCore 安装、更新和卸载流程。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Result, bail};
use chrono::Local;
use uuid::Uuid;

use super::cleanup::{clean_engine_ini, clean_legacy_range_mod};
use super::filesystem::{copy_path, delete_path, write_json_file};
use super::pathing::validate_win64_path;
use super::payload::{
    collect_stats, download_project_rebound_release, extract_managed_item,
    is_project_rebound_online_file, open_payload_archive, read_project_rebound_release_files,
    write_project_rebound_release_item,
};
use super::util::{ensure_dir, iso_now};
use super::*;

impl InstallerCore {
    /// 在触碰目标文件前确认内嵌载荷条目齐全。
    pub fn validate_payload(&self) -> Result<()> {
        let mut archive = open_payload_archive()?;
        for item in MANAGED_ITEMS {
            if is_project_rebound_online_file(item.name) {
                continue;
            }
            let exists = match item.kind {
                ItemKind::File => archive.by_name(item.name).is_ok(),
                ItemKind::Dir => archive
                    .file_names()
                    .any(|name| name == item.name || name.starts_with(&format!("{}/", item.name))),
            };
            if !exists {
                bail!("内嵌载荷缺少：{}", item.name);
            }
        }
        Ok(())
    }

    /// 将工具箱载荷安装或更新到所选游戏目录。
    ///
    /// 在线 ProjectRebound 包会先下载并完整校验，然后才替换目标文件。
    /// 已存在且不属于安装器的文件只备份一次，并记录进 state.json 供后续恢复。
    pub fn install(&self, target_win64: &Path, keep_topmost: bool, hotkey: &str) -> Result<String> {
        self.validate_payload()?;
        validate_win64_path(target_win64)?;
        self.log(format!(
            "下载 ProjectRebound 在线版本：{}",
            PROJECT_REBOUND_RELEASE_URL
        ));
        let project_rebound_release = download_project_rebound_release()?;
        let project_rebound_files = read_project_rebound_release_files(&project_rebound_release)?;

        let existing_state = self.load_state(target_win64)?.unwrap_or_default();
        let existing_markers = self.load_markers(target_win64)?.unwrap_or_default();

        // 运行时文件会被游戏/服务加载，因此备份或原子替换前先停止相关进程。
        self.stop_runtime_processes_before_install(target_win64)?;
        let legacy_removed = clean_legacy_range_mod(target_win64, self.logger.clone())?;
        let cleaned = clean_engine_ini(self.logger.clone())?;
        let install_id = format!(
            "{}_{}",
            Local::now().format("%Y%m%d_%H%M%S"),
            &Uuid::new_v4().simple().to_string()[..8]
        );
        let paths = self.metadata_paths(target_win64);
        ensure_dir(&paths.metadata_dir)?;
        let backup_root = paths.backups_root.join(&install_id);
        ensure_dir(&backup_root)?;

        let existing_map: HashMap<String, ManagedRecord> = existing_state
            .managed_items
            .iter()
            .cloned()
            .map(|item| (item.name.clone(), item))
            .collect();
        let existing_marker_set: HashSet<String> =
            existing_markers.managed_names.into_iter().collect();

        let mut managed_records = Vec::new();
        for item in MANAGED_ITEMS {
            let target_path = target_win64.join(item.name);
            let existing_record = existing_map.get(item.name);
            let installer_managed_before = existing_marker_set.contains(item.name);
            let existed_before_install = target_path.exists();
            let mut backup_relative =
                existing_record.and_then(|record| record.backup_relative.clone());

            if target_path.exists() && !installer_managed_before {
                // 只备份安装器接管前就存在的文件；已标记为安装器管理的文件可直接覆盖，
                // 避免每次更新都产生备份链。
                backup_relative = Some(self.backup_item(&target_path, &backup_root, item.name)?);
            }

            let source_path = if is_project_rebound_online_file(item.name) {
                self.log(format!(
                    "写入在线 ProjectRebound 文件：{} -> {}",
                    item.name,
                    target_path.display()
                ));
                write_project_rebound_release_item(
                    &project_rebound_files,
                    item.name,
                    &target_path,
                )?;
                format!("{}#{}", PROJECT_REBOUND_RELEASE_URL, item.name)
            } else {
                // 内嵌条目先删除再解压，因为目录无法用文件写入替换，且可能残留旧子项。
                if target_path.exists() {
                    self.log(format!("覆盖目标内容：{}", target_path.display()));
                    delete_path(&target_path)?;
                }
                self.log(format!(
                    "复制内嵌载荷：{} -> {}",
                    item.name,
                    target_path.display()
                ));
                extract_managed_item(item, target_win64)?;
                format!("embedded://{}", item.name)
            };

            // 写入完成后再采集状态，确保 size/hash 对应实际安装内容。
            let stats = collect_stats(&target_path)?;
            managed_records.push(ManagedRecord {
                name: item.name.to_string(),
                item_type: match item.kind {
                    ItemKind::File => "file".to_string(),
                    ItemKind::Dir => "dir".to_string(),
                },
                target_path: target_path.display().to_string(),
                source_path,
                installed_at: iso_now(),
                existed_before_install,
                backup_relative,
                installer_managed_before,
                size: stats.size,
                sha256: stats.sha256,
                file_count: stats.file_count,
                dir_count: stats.dir_count,
            });
        }

        let topmost = self.write_topmost_config(target_win64, keep_topmost, hotkey)?;
        let state = InstallState {
            version: APP_VERSION.to_string(),
            app_id: APP_ID.to_string(),
            install_id: install_id.clone(),
            source_root: format!("embedded://payload.zip + {}", PROJECT_REBOUND_RELEASE_URL),
            target_dir: target_win64.display().to_string(),
            installed_at: if existing_state.installed_at.is_empty() {
                iso_now()
            } else {
                existing_state.installed_at
            },
            updated_at: iso_now(),
            managed_items: managed_records,
            topmost_config: topmost.clone(),
        };
        write_json_file(&paths.state_file, &state)?;
        self.write_markers(target_win64, install_id)?;
        self.log(format!("安装状态已写入：{}", paths.state_file.display()));

        let mut notes = vec![
            "安装完成。".to_string(),
            format!("窗口置顶目标：{}（固定）", TOPMOST_GAME_LABEL),
            format!(
                "持续置顶：{}",
                if topmost.keep_topmost {
                    "已开启"
                } else {
                    "已关闭"
                }
            ),
            format!("持续置顶开关键：{}", topmost.hotkey),
            "原版启动脚本：startgame.bat".to_string(),
            "窗口置顶功能仅由 Rust 工具箱负责，不修改 startgame.bat。".to_string(),
            "Payload.dll 和 ProjectReboundServerWrapper.exe 已从在线 Nightly Release 更新。"
                .to_string(),
        ];
        if !legacy_removed.is_empty() {
            notes.push(format!(
                "已清理旧靶场模组残留 {} 项。",
                legacy_removed.len()
            ));
        }
        if let Some(path) = cleaned {
            notes.push(format!("并已清理冲突配置：{}", path.display()));
        }
        Ok(notes.join("\n"))
    }

    /// 在 state.json 完整时移除安装文件并恢复备份。
    pub fn uninstall(&self, target_win64: &Path) -> Result<String> {
        validate_win64_path(target_win64)?;
        let paths = self.metadata_paths(target_win64);
        let state = self.load_state(target_win64)?;
        let markers = self.load_markers(target_win64)?;
        let cleaned = clean_engine_ini(self.logger.clone())?;

        if let Some(state) = state {
            self.log("检测到 state.json，执行完整卸载。");
            // 按安装记录反向处理，避免先删父目录导致子文件无法处理。
            for item in state.managed_items.iter().rev() {
                if !MANAGED_ITEMS
                    .iter()
                    .any(|managed| managed.name == item.name)
                {
                    continue;
                }
                let target_path = target_win64.join(&item.name);
                if target_path.exists() {
                    self.log(format!("删除已安装内容：{}", target_path.display()));
                    delete_path(&target_path)?;
                }
                if let Some(backup_relative) = &item.backup_relative {
                    let backup_path = target_win64.join(METADATA_DIR_NAME).join(backup_relative);
                    if backup_path.exists() {
                        self.log(format!(
                            "恢复备份：{} -> {}",
                            backup_path.display(),
                            target_path.display()
                        ));
                        copy_path(&backup_path, &target_path)?;
                    } else {
                        self.log(format!(
                            "警告：找不到备份，无法恢复 {}：{}",
                            item.name,
                            backup_path.display()
                        ));
                    }
                }
            }
            if paths.metadata_dir.exists() {
                self.log(format!(
                    "删除安装器元数据目录：{}",
                    paths.metadata_dir.display()
                ));
                delete_path(&paths.metadata_dir)?;
            }
            let mut message = "卸载完成。".to_string();
            if let Some(path) = cleaned {
                message.push_str(&format!("\n并已清理冲突配置：{}", path.display()));
            }
            return Ok(message);
        }

        // 对 markers.json 的能力刻意做了限制：只允许删除已知文件，不做恢复，
        // 因为备份关系已经丢失。
        let marker_names: Vec<String> = markers
            .unwrap_or_default()
            .managed_names
            .into_iter()
            .filter(|name| MANAGED_ITEMS.iter().any(|item| item.name == name))
            .collect();
        if marker_names.is_empty() {
            bail!("未找到可用于卸载的 state.json 或 markers.json，无法安全判断已安装内容。");
        }

        self.log("state.json 缺失，进入受限卸载模式。");
        for name in marker_names {
            let target_path = target_win64.join(&name);
            if target_path.exists() {
                self.log(format!("受限卸载删除：{}", target_path.display()));
                delete_path(&target_path)?;
            }
        }

        let mut warning = "state.json 缺失，已按受管标记删除安装内容，但无法恢复历史备份，请手动检查 .pve_installer 目录。".to_string();
        if let Some(path) = cleaned {
            warning.push_str(&format!("\n并已清理冲突配置：{}", path.display()));
        }
        Ok(warning)
    }
}
