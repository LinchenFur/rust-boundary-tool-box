//! 核心安装、启动、检测和清理逻辑。
//!
//! 这个模块集中负责文件系统变更和进程操作。Slint UI 只调用 `InstallerCore`，
//! 替换文件、恢复备份、结束进程、读取 Steam 元数据、校验在线下载等高风险动作
//! 都收敛在这里处理。

use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Cursor, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::Local;
use netstat2::{AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, TcpState, get_sockets_info};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sysinfo::System;
use uuid::Uuid;
use walkdir::WalkDir;
use windows::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
use windows::core::PCWSTR;
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use zip::ZipArchive;

use crate::win::{parse_hotkey_text, watch_window_by_pid};

/// 显示在 UI 中并写入安装元数据的应用版本。
pub const APP_VERSION: &str = "1.2.0";
/// 游戏 Boundary 的 Steam App ID。
pub const APP_ID: &str = "1364020";
/// 用于校验 Binaries\Win64 目录的游戏主程序。
pub const GAME_EXE: &str = "ProjectBoundarySteam-Win64-Shipping.exe";
/// 可执行文件旁边用于存放日志和安装器数据的目录名。
pub const INSTALLER_FOLDER_NAME: &str = "installer_tool";
/// 目标 Win64 目录内创建的元数据目录名。
pub const METADATA_DIR_NAME: &str = ".pve_installer";
pub const STATE_FILE_NAME: &str = "state.json";
pub const MARKERS_FILE_NAME: &str = "markers.json";
pub const TOPMOST_MODE_FILE_NAME: &str = "topmost_mode.txt";
pub const TOPMOST_KEEP_FILE_NAME: &str = "topmost_keep.txt";
pub const TOPMOST_HOTKEY_FILE_NAME: &str = "topmost_hotkey.txt";
pub const DEFAULT_TOPMOST_MODE: &str = "game";
pub const DEFAULT_KEEP_TOPMOST: bool = true;
pub const DEFAULT_TOPMOST_HOTKEY: &str = "Ctrl+Alt+F10";
pub const TOPMOST_GAME_LABEL: &str = "Boundary Game 游戏窗口";
pub const TOPMOST_WATCH_START_TIMEOUT_SECONDS: u64 = 180;
pub const TOPMOST_WATCH_LOST_TIMEOUT_SECONDS: u64 = 15;
/// 在 Windows 上隐藏辅助进程窗口所用的 CREATE_NO_WINDOW 标志。
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 用于两个 ProjectRebound 运行时二进制的 Nightly 包地址。
pub const PROJECT_REBOUND_RELEASE_URL: &str = "https://git-proxy.cubland.icu/https://github.com/STanJK/ProjectRebound/releases/download/Nightly/Release.zip";
/// 这些文件刻意从线上获取，而不是放进 payload.zip。
pub const PROJECT_REBOUND_ONLINE_FILES: &[&str] =
    &["Payload.dll", "ProjectReboundServerWrapper.exe"];
/// 本地登录/游戏服务需要的 TCP 端口。
pub const REQUIRED_TCP_PORTS: &[u16] = &[6969, 7777, 8000, 9000];
/// 本地游戏服务需要的 UDP 端口。
pub const REQUIRED_UDP_PORTS: &[u16] = &[7777, 9000];
/// 诊断页展示的端口行。
pub const MONITORED_PORTS: &[(&str, u16)] = &[
    ("TCP", 6969),
    ("TCP", 7777),
    ("UDP", 7777),
    ("TCP", 8000),
    ("TCP", 9000),
    ("UDP", 9000),
];

// 由 build.rs 生成。公开纯源码构建时它可能为空，因此安装前必须由
// 由 validate_payload() 校验必要条目。
const PAYLOAD_ZIP_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.zip"));

/// 受管载荷条目的类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    File,
    Dir,
}

/// 安装器在 Binaries\Win64 内负责管理的文件或目录。
#[derive(Clone, Copy, Debug)]
pub struct ManagedItem {
    pub name: &'static str,
    pub kind: ItemKind,
}

/// 工具箱会安装或移除的完整文件/目录集合。
pub const MANAGED_ITEMS: &[ManagedItem] = &[
    ManagedItem {
        name: "BoundaryMetaServer-main",
        kind: ItemKind::Dir,
    },
    ManagedItem {
        name: "nodejs",
        kind: ItemKind::Dir,
    },
    ManagedItem {
        name: "commandlist.txt",
        kind: ItemKind::File,
    },
    ManagedItem {
        name: "DT_ItemType.json",
        kind: ItemKind::File,
    },
    ManagedItem {
        name: "dxgi.dll",
        kind: ItemKind::File,
    },
    ManagedItem {
        name: "Payload.dll",
        kind: ItemKind::File,
    },
    ManagedItem {
        name: "ProjectReboundServerWrapper.exe",
        kind: ItemKind::File,
    },
    ManagedItem {
        name: "startgame.bat",
        kind: ItemKind::File,
    },
    ManagedItem {
        name: "steam_appid.txt",
        kind: ItemKind::File,
    },
];

// 旧 Python/模组包残留。名称固定且范围限定在 Boundary 安装树内，因此可以清理。
const OLD_MOD_LOGICMOD_FILES: &[&str] =
    &["oneMOD.pak", "oneMOD20250914.pak", "解锁armorymanager.TXT"];
const OLD_MOD_UE4SS_MOD_ENTRIES: &[&str] = &[
    "ActorDumperMod",
    "BPModLoaderMod",
    "CheatManagerEnablerMod",
    "ConsoleCommandsMod",
    "ConsoleEnablerMod",
    "Keybinds",
    "LineTraceMod",
    "SplitScreenMod",
    "shared",
    "Changelog.md",
    "mods.txt",
    "Readme.md",
];
const OLD_MOD_UE4SS_SUPPORT_FILES: &[&str] = &[
    "Changelog.md",
    "QQ20241229-164840.png",
    "Readme.md",
    "UE4SS-settings.ini",
];
const OLD_MOD_UE4SS_LOADER_FILES: &[&str] = &["xinput1_3.dll"];

pub type Logger = Arc<dyn Fn(String) + Send + Sync + 'static>;

/// 写入 state.json 的逐项安装记录，用于精确卸载和恢复。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ManagedRecord {
    pub name: String,
    pub item_type: String,
    pub target_path: String,
    pub source_path: String,
    pub installed_at: String,
    pub existed_before_install: bool,
    pub backup_relative: Option<String>,
    pub installer_managed_before: bool,
    pub size: u64,
    pub sha256: Option<String>,
    pub file_count: Option<u64>,
    pub dir_count: Option<u64>,
}

/// 与安装状态一起持久化的窗口置顶配置。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TopmostConfig {
    pub mode: String,
    pub keep_topmost: bool,
    pub hotkey: String,
}

/// 完整安装元数据，也是完整卸载的权威来源。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InstallState {
    pub version: String,
    pub app_id: String,
    pub install_id: String,
    pub source_root: String,
    pub target_dir: String,
    pub installed_at: String,
    pub updated_at: String,
    pub managed_items: Vec<ManagedRecord>,
    #[serde(default)]
    pub topmost_config: TopmostConfig,
}

/// 当 state.json 不可用时使用的最小兜底元数据。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InstallMarkers {
    pub version: String,
    pub install_id: String,
    pub managed_names: Vec<String>,
    pub target_dir: String,
    pub updated_at: String,
}

/// 从 sysinfo 获取的进程行，用于 UI 诊断和清理。
#[derive(Debug, Clone, Default)]
pub struct RuntimeProcess {
    pub pid: u32,
    pub name: String,
    pub exe: String,
    pub cmd: String,
}

/// 属于当前 Boundary 安装目录的运行时进程分组。
#[derive(Debug, Clone, Default)]
pub struct RuntimeSnapshot {
    pub game: Vec<RuntimeProcess>,
    pub wrapper: Vec<RuntimeProcess>,
    pub server: Vec<RuntimeProcess>,
    pub watcher: Vec<RuntimeProcess>,
}

/// 绑定了必要本地端口的进程。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortConflict {
    pub protocol: String,
    pub port: u16,
    pub pid: u32,
    pub name: String,
    pub exe: String,
}

/// 单个监控端口的 UI 友好状态。
#[derive(Debug, Clone)]
pub struct PortStatusRow {
    pub protocol: &'static str,
    pub port: u16,
    pub conflict: Option<PortConflict>,
}

/// 启动社区服和游戏所需的路径集合。
#[derive(Debug, Clone)]
pub struct LaunchFiles {
    pub server_dir: PathBuf,
    pub node_exe: PathBuf,
    pub wrapper_exe: PathBuf,
    pub game_exe: PathBuf,
}

/// 在 UI 中选择的目录发现模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathMode {
    Auto,
    Manual,
}

/// 从一个目标 Binaries\Win64 目录推导出的全部元数据路径。
#[derive(Debug, Clone)]
struct MetadataPaths {
    metadata_dir: PathBuf,
    state_file: PathBuf,
    markers_file: PathBuf,
    topmost_mode_file: PathBuf,
    topmost_keep_file: PathBuf,
    topmost_hotkey_file: PathBuf,
    backups_root: PathBuf,
}

/// 供 UI 使用的主服务对象。
///
/// 由于 logger 使用引用计数，克隆成本很低。耗时工作仍由 UI 层放到后台线程执行。
#[derive(Clone)]
pub struct InstallerCore {
    pub runtime_dir: PathBuf,
    pub installer_home: PathBuf,
    logger: Logger,
}

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

    /// 解析启动所需全部文件，并集中报告缺失路径。
    pub fn validate_launch_files(&self, target_win64: &Path) -> Result<LaunchFiles> {
        let files = launch_files(target_win64);
        let missing: Vec<String> = [
            &files.server_dir,
            &files.node_exe,
            &files.wrapper_exe,
            &files.game_exe,
        ]
        .iter()
        .filter(|path| !path.exists())
        .map(|path| path.display().to_string())
        .collect();
        if !missing.is_empty() {
            bail!("缺少启动所需文件：\n- {}", missing.join("\n- "));
        }
        Ok(files)
    }

    /// 仅收集属于所选目标目录的 Boundary 运行时进程。
    ///
    /// 匹配范围刻意限定为可执行路径或命令行包含目标目录，避免误杀其它目录下
    /// 运行 index.js 的无关 node.exe 服务。
    pub fn collect_runtime_processes(&self, target_win64: &Path) -> Result<RuntimeSnapshot> {
        let files = launch_files(target_win64);
        let game_exe = path_match_key(&files.game_exe);
        let wrapper_exe = path_match_key(&files.wrapper_exe);
        let node_exe = path_match_key(&files.node_exe);
        let target_dir = path_match_key(target_win64);
        let watcher_exe = path_match_key(&env::current_exe()?);

        let mut system = System::new_all();
        system.refresh_all();
        let mut snapshot = RuntimeSnapshot::default();
        for process in system.processes().values() {
            let exe_lower = process
                .exe()
                .map(|path| path.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let name_lower = process.name().to_string_lossy().to_lowercase();
            let cmd = process
                .cmd()
                .iter()
                .map(|part| part.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ");
            let cmd_lower = cmd.to_lowercase();
            let item = RuntimeProcess {
                pid: process.pid().as_u32(),
                name: process.name().to_string_lossy().into_owned(),
                exe: process
                    .exe()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
                cmd,
            };
            let launched_from_target =
                exe_lower.contains(&target_dir) || cmd_lower.contains(&target_dir);

            // 优先使用精确可执行路径；只有命令行能证明进程属于当前安装时，
            // 才退回到进程名匹配。
            if exe_lower == game_exe
                || (name_lower == GAME_EXE.to_ascii_lowercase() && launched_from_target)
            {
                snapshot.game.push(item);
            } else if exe_lower == wrapper_exe
                || (name_lower == "projectreboundserverwrapper.exe" && launched_from_target)
            {
                snapshot.wrapper.push(item);
            } else if exe_lower == node_exe || (name_lower == "node.exe" && launched_from_target) {
                snapshot.server.push(item);
            } else if exe_lower == watcher_exe && cmd_lower.contains("--watch-pid") {
                snapshot.watcher.push(item);
            }
        }
        Ok(snapshot)
    }

    /// 返回当前被占用的必要端口。
    pub fn collect_port_conflicts(&self) -> Result<Vec<PortConflict>> {
        collect_port_conflicts()
    }

    /// 为 UI 端口列表构造固定顺序的诊断行。
    pub fn port_status_rows(&self) -> Result<Vec<PortStatusRow>> {
        let conflicts = self.collect_port_conflicts()?;
        let mut map = HashMap::new();
        for conflict in conflicts {
            map.insert((conflict.protocol.to_uppercase(), conflict.port), conflict);
        }
        let rows = MONITORED_PORTS
            .iter()
            .map(|(protocol, port)| PortStatusRow {
                protocol,
                port: *port,
                conflict: map.get(&(protocol.to_string(), *port)).cloned(),
            })
            .collect();
        Ok(rows)
    }

    /// 结束占用必要端口的进程。
    pub fn stop_port_conflict_processes(&self, conflicts: &[PortConflict]) -> Result<String> {
        let mut pids = Vec::new();
        for conflict in conflicts {
            if conflict.pid > 0 && !pids.contains(&conflict.pid) {
                pids.push(conflict.pid);
            }
        }
        if pids.is_empty() {
            return Ok("未找到可关闭的占用进程。".to_string());
        }
        kill_pids(&pids)?;
        self.log(format!(
            "端口占用清理：已请求结束 PID {}",
            pids.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ));
        Ok(format!(
            "已关闭以下端口占用进程：\n{}",
            format_port_conflicts(conflicts)
        ))
    }

    /// 结束游戏、服务包装器、登录服务器和置顶守护进程。
    pub fn stop_runtime_processes(&self, target_win64: &Path) -> Result<String> {
        let snapshot = self.collect_runtime_processes(target_win64)?;
        let mut pids = Vec::new();
        for group in [
            &snapshot.watcher,
            &snapshot.game,
            &snapshot.wrapper,
            &snapshot.server,
        ] {
            for process in group.iter() {
                if process.pid > 0 && !pids.contains(&process.pid) {
                    pids.push(process.pid);
                }
            }
        }
        if pids.is_empty() {
            self.log("关闭所有进程：未检测到需要关闭的相关进程。");
            return Ok("当前没有需要关闭的相关进程。".to_string());
        }
        kill_pids(&pids)?;
        self.log(format!(
            "关闭所有进程：已请求结束 PID {}",
            pids.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ));
        Ok(format!(
            "已关闭相关进程：\n{}",
            summarize_runtime_processes(&snapshot)
        ))
    }

    /// 安装前尽力关闭进程，并在结束后再次校验。
    fn stop_runtime_processes_before_install(&self, target_win64: &Path) -> Result<()> {
        let snapshot = self.collect_runtime_processes(target_win64)?;
        let pids = runtime_process_pids(&snapshot);
        if pids.is_empty() {
            self.log("安装前检查：未检测到正在运行的相关进程。");
            return Ok(());
        }

        self.log(format!(
            "安装前关闭相关进程：{}",
            summarize_runtime_processes(&snapshot)
        ));
        let kill_failures = taskkill_pids(&pids)?;

        for _ in 0..20 {
            thread::sleep(Duration::from_millis(250));
            let latest = self.collect_runtime_processes(target_win64)?;
            if runtime_process_pids(&latest).is_empty() {
                if !kill_failures.is_empty() {
                    self.log(format!(
                        "安装前关闭相关进程：taskkill 返回失败但目标进程已退出：{}",
                        kill_failures.join("；")
                    ));
                }
                self.log("安装前关闭相关进程：已全部退出。");
                return Ok(());
            }
        }

        let latest = self.collect_runtime_processes(target_win64)?;
        let failure_text = if kill_failures.is_empty() {
            String::new()
        } else {
            format!("\ntaskkill 失败详情：{}", kill_failures.join("；"))
        };
        bail!(
            "安装前仍有相关进程未退出，请手动关闭后重试：{}{}",
            summarize_runtime_processes(&latest),
            failure_text
        )
    }

    /// 启动登录服务器、ProjectRebound 包装器、游戏和置顶守护。
    pub fn launch(&self, target_win64: &Path, keep_topmost: bool, hotkey: &str) -> Result<String> {
        validate_win64_path(target_win64)?;
        let files = self.validate_launch_files(target_win64)?;
        let cleaned = clean_engine_ini(self.logger.clone())?;
        let topmost = self.write_topmost_config(target_win64, keep_topmost, hotkey)?;

        self.log(format!("启动登录服务器：{}", files.node_exe.display()));
        hidden_command(&files.node_exe)
            .current_dir(&files.server_dir)
            .arg("index.js")
            .spawn()
            .context("启动登录服务器失败")?;
        thread::sleep(Duration::from_secs(5));

        self.log(format!("启动服务包装器：{}", files.wrapper_exe.display()));
        hidden_command(&files.wrapper_exe)
            .current_dir(target_win64)
            .spawn()
            .context("启动服务包装器失败")?;
        thread::sleep(Duration::from_secs(2));

        self.log(format!("启动游戏：{}", files.game_exe.display()));
        let game_process = Command::new(&files.game_exe)
            .current_dir(target_win64)
            .arg("-LogicServerURL=http://127.0.0.1:8000")
            .spawn()
            .context("启动游戏失败")?;

        self.log("Rust 置顶守护：目标固定为游戏窗口。");
        let mut watcher = hidden_command(env::current_exe()?);
        watcher
            .arg("--watch-pid")
            .arg(game_process.id().to_string())
            .arg("--hotkey")
            .arg(topmost.hotkey.clone());
        if topmost.keep_topmost {
            watcher.arg("--keep-topmost");
        }
        watcher.spawn().context("启动置顶守护失败")?;

        let mut notes = vec![
            "启动完成。".to_string(),
            format!("窗口置顶目标：{}", TOPMOST_GAME_LABEL),
            if topmost.keep_topmost {
                "持续置顶：默认已开启，按开关键可关闭或重新开启".to_string()
            } else {
                "持续置顶：默认已关闭，按开关键可开启或再次关闭".to_string()
            },
            format!("持续置顶开关键：{}", topmost.hotkey),
            "原版批处理仍保留为 startgame.bat，未被修改参与该功能。".to_string(),
        ];
        if let Some(path) = cleaned {
            notes.push(format!("并已清理冲突配置：{}", path.display()));
        }
        Ok(notes.join("\n"))
    }

    /// 计算所选安装目录对应的元数据位置。
    fn metadata_paths(&self, target_win64: &Path) -> MetadataPaths {
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
    fn backup_item(
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
    fn write_markers(&self, target_win64: &Path, install_id: String) -> Result<()> {
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
    fn load_state(&self, target_win64: &Path) -> Result<Option<InstallState>> {
        read_json_file(&self.metadata_paths(target_win64).state_file)
    }

    /// 读取最小安装标记。文件缺失返回 Ok(None)，JSON 损坏视为错误。
    fn load_markers(&self, target_win64: &Path) -> Result<Option<InstallMarkers>> {
        read_json_file(&self.metadata_paths(target_win64).markers_file)
    }
}

pub(crate) mod cleanup;
pub(crate) mod filesystem;
pub(crate) mod pathing;
pub(crate) mod payload;
pub(crate) mod process;
pub(crate) mod util;

pub use pathing::{list_available_drives, normalize_selected_path, validate_win64_path};
pub use process::{format_port_conflicts, summarize_runtime_processes};
pub use util::{
    iso_now, normalize_hotkey, normalize_keep_topmost, normalize_topmost_mode, now_text,
    watch_mode_from_args,
};

use cleanup::{clean_engine_ini, clean_legacy_range_mod};
use filesystem::{copy_path, delete_path, read_json_file, write_json_file};
use pathing::{detect_steam_game_win64, scan_drives_for_game};
use payload::{
    collect_stats, download_project_rebound_release, extract_managed_item,
    is_project_rebound_online_file, open_payload_archive, read_project_rebound_release_files,
    write_project_rebound_release_item,
};
use process::{
    collect_port_conflicts, kill_pids, launch_files, path_match_key, runtime_process_pids,
    taskkill_pids,
};
use util::{ensure_dir, hidden_command, resolve_installer_home};
