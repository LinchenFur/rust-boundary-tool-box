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

#[derive(Debug, Clone)]
struct ItemStats {
    size: u64,
    sha256: Option<String>,
    file_count: Option<u64>,
    dir_count: Option<u64>,
}

/// 供 UI 和日志使用的本地时间戳。
pub fn now_text() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 写入元数据的紧凑 ISO 风格时间戳。
pub fn iso_now() -> String {
    Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

/// 当前置顶目标固定为游戏窗口。
pub fn normalize_topmost_mode(_value: &str) -> String {
    DEFAULT_TOPMOST_MODE.to_string()
}

/// 解析置顶开关持久化字符串中的真假值。
pub fn normalize_keep_topmost(value: impl ToString) -> bool {
    !matches!(
        value.to_string().trim().to_lowercase().as_str(),
        "" | "0" | "false" | "no" | "off"
    )
}

/// 校验并规范化用户输入或捕获到的快捷键。
pub fn normalize_hotkey(value: impl AsRef<str>) -> Result<String> {
    Ok(parse_hotkey_text(value.as_ref())?.normalized)
}

/// 处理隐藏置顶守护子进程的命令行模式。
pub fn watch_mode_from_args(args: &[String]) -> Option<Result<i32>> {
    let pid = cli_value(args, "--watch-pid")?.parse::<u32>().ok()?;
    let hotkey = cli_value(args, "--hotkey").unwrap_or_else(|| DEFAULT_TOPMOST_HOTKEY.to_string());
    let keep_topmost = args.iter().any(|item| item == "--keep-topmost");
    Some(watch_window_by_pid(pid, keep_topmost, &hotkey))
}

/// 读取命令行标志后面的值。
fn cli_value(args: &[String], flag: &str) -> Option<String> {
    let index = args.iter().position(|item| item == flag)?;
    args.get(index + 1).cloned()
}

/// 将安装器数据放在可执行文件旁边或 installer_tool 目录下。
fn resolve_installer_home(runtime_dir: &Path) -> PathBuf {
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
fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    Ok(())
}

/// 创建无控制台窗口且不继承标准输入输出的 Windows 命令。
fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// 单独封装 taskkill，便于统一参数和后续测试。
fn hidden_taskkill_command() -> Command {
    hidden_command("taskkill")
}

/// 打开构建期内嵌的载荷压缩包。
fn open_payload_archive() -> Result<ZipArchive<Cursor<&'static [u8]>>> {
    ZipArchive::new(Cursor::new(PAYLOAD_ZIP_BYTES)).context("无法读取内嵌载荷")
}

/// 将一个受管内嵌文件或目录解压到目标根目录。
fn extract_managed_item(item: &ManagedItem, target_root: &Path) -> Result<()> {
    let mut archive = open_payload_archive()?;
    match item.kind {
        ItemKind::File => {
            let mut entry = archive
                .by_name(item.name)
                .with_context(|| format!("内嵌载荷缺少文件 {}", item.name))?;
            let target = target_root.join(item.name);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut output = File::create(&target)?;
            io::copy(&mut entry, &mut output)?;
        }
        ItemKind::Dir => {
            let prefix = format!("{}/", item.name);
            let names: Vec<String> = archive
                .file_names()
                .filter(|name| *name == item.name || name.starts_with(&prefix))
                .map(ToOwned::to_owned)
                .collect();
            if names.is_empty() {
                bail!("内嵌载荷缺少目录 {}", item.name);
            }
            for name in names {
                let mut entry = archive.by_name(&name)?;
                let out_path = target_root.join(Path::new(&name));
                if entry.is_dir() {
                    fs::create_dir_all(&out_path)?;
                    continue;
                }
                if let Some(parent) = out_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut output = File::create(&out_path)?;
                io::copy(&mut entry, &mut output)?;
            }
        }
    }
    Ok(())
}

/// 判断文件是否来自在线 ProjectRebound release。
fn is_project_rebound_online_file(name: &str) -> bool {
    PROJECT_REBOUND_ONLINE_FILES
        .iter()
        .any(|item| item.eq_ignore_ascii_case(name))
}

/// 下载当前 ProjectRebound Nightly zip。
fn download_project_rebound_release() -> Result<Vec<u8>> {
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(90))
        .user_agent(format!("boundary-toolbox/{}", APP_VERSION))
        .build()?
        .get(PROJECT_REBOUND_RELEASE_URL)
        .send()
        .context("下载 ProjectRebound Nightly Release 失败")?
        .error_for_status()
        .context("ProjectRebound Nightly Release 返回错误状态")?;
    let bytes = response
        .bytes()
        .context("读取 ProjectRebound Nightly Release 内容失败")?;
    Ok(bytes.to_vec())
}

/// 预检在线 zip，并把所有必需文件读入内存。
///
/// 替换前先把字节保存在内存里，可以防止代理返回 HTML、损坏 zip、
/// 或 release 缺少文件时留下半安装状态。
fn read_project_rebound_release_files(release_zip: &[u8]) -> Result<HashMap<String, Vec<u8>>> {
    let mut archive = ZipArchive::new(Cursor::new(release_zip))
        .context("无法读取 ProjectRebound Nightly Release 压缩包")?;
    let mut files = HashMap::new();
    for item_name in PROJECT_REBOUND_ONLINE_FILES {
        let bytes = read_project_rebound_release_file(&mut archive, item_name)?;
        files.insert(item_name.to_string(), bytes);
    }
    Ok(files)
}

/// 在 ProjectRebound zip 内按文件名查找一个必需文件。
fn read_project_rebound_release_file(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    item_name: &str,
) -> Result<Vec<u8>> {
    let entry_name = archive
        .file_names()
        .find(|name| {
            Path::new(name).file_name().is_some_and(|file_name| {
                file_name.to_string_lossy().eq_ignore_ascii_case(item_name)
            })
        })
        .map(ToOwned::to_owned)
        .with_context(|| format!("ProjectRebound Nightly Release 缺少 {}", item_name))?;
    let mut entry = archive
        .by_name(&entry_name)
        .with_context(|| format!("无法读取 ProjectRebound 文件 {}", entry_name))?;
    if entry.is_dir() {
        bail!("ProjectRebound Nightly Release 中的 {} 不是文件", item_name);
    }
    let mut bytes = Vec::new();
    entry
        .read_to_end(&mut bytes)
        .with_context(|| format!("读取 ProjectRebound 文件 {} 失败", entry_name))?;
    if bytes.is_empty() {
        bail!("ProjectRebound Nightly Release 中的 {} 是空文件", item_name);
    }
    Ok(bytes)
}

/// 将已校验的 ProjectRebound 文件写入最终目标路径。
fn write_project_rebound_release_item(
    files: &HashMap<String, Vec<u8>>,
    item_name: &str,
    target_path: &Path,
) -> Result<()> {
    let bytes = files
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(item_name))
        .map(|(_, bytes)| bytes.as_slice())
        .with_context(|| format!("ProjectRebound Nightly Release 缺少 {}", item_name))?;
    replace_file_with_bytes(target_path, bytes)
        .with_context(|| format!("写入在线 ProjectRebound 文件 {} 失败", item_name))
}

/// 写入同目录临时文件后，原子替换最终文件。
fn replace_file_with_bytes(target_path: &Path, bytes: &[u8]) -> Result<()> {
    if target_path.exists() && target_path.is_dir() {
        bail!("目标路径是目录，无法按文件替换：{}", target_path.display());
    }
    let parent = target_path
        .parent()
        .with_context(|| format!("目标路径缺少父目录：{}", target_path.display()))?;
    fs::create_dir_all(parent)?;
    let temp_path = temp_file_for_target(target_path)?;
    {
        let mut output = File::create(&temp_path)
            .with_context(|| format!("创建临时文件失败：{}", temp_path.display()))?;
        output
            .write_all(bytes)
            .with_context(|| format!("写入临时文件失败：{}", temp_path.display()))?;
        output
            .sync_all()
            .with_context(|| format!("刷新临时文件失败：{}", temp_path.display()))?;
    }

    // 使用带 WRITE_THROUGH 的 MoveFileExW，比 remove+rename 更可靠，
    // 普通失败时不会留下目标文件缺失状态。
    if let Err(error) = move_file_replace(&temp_path, target_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    Ok(())
}

/// 在目标旁边生成唯一临时路径，确保替换发生在同一卷内。
fn temp_file_for_target(target_path: &Path) -> Result<PathBuf> {
    let parent = target_path
        .parent()
        .with_context(|| format!("目标路径缺少父目录：{}", target_path.display()))?;
    let file_name = target_path
        .file_name()
        .with_context(|| format!("目标路径缺少文件名：{}", target_path.display()))?
        .to_string_lossy();
    Ok(parent.join(format!(".{}.{}.tmp", file_name, Uuid::new_v4().simple())))
}

/// 执行 Windows 原子替换。
fn move_file_replace(source: &Path, target: &Path) -> Result<()> {
    let source_wide = path_to_wide(source);
    let target_wide = path_to_wide(target);
    unsafe {
        MoveFileExW(
            PCWSTR(source_wide.as_ptr()),
            PCWSTR(target_wide.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .with_context(|| format!("替换文件失败：{} -> {}", source.display(), target.display()))
}

/// 将 Rust 路径转换为以 0 结尾的 UTF-16 Windows 字符串。
fn path_to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// 流式计算文件 SHA-256，避免完整载入大文件。
fn compute_file_sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// 为安装记录采集大小、哈希和文件数量元数据。
fn collect_stats(path: &Path) -> Result<ItemStats> {
    if path.is_file() {
        return Ok(ItemStats {
            size: path.metadata()?.len(),
            sha256: Some(compute_file_sha256(path)?),
            file_count: None,
            dir_count: None,
        });
    }

    let mut total_size = 0_u64;
    let mut file_count = 0_u64;
    let mut dir_count = 0_u64;
    for entry in WalkDir::new(path) {
        let entry = entry?;
        if entry.file_type().is_file() {
            file_count += 1;
            total_size += entry.metadata()?.len();
        } else if entry.file_type().is_dir() && entry.path() != path {
            dir_count += 1;
        }
    }
    Ok(ItemStats {
        size: total_size,
        sha256: None,
        file_count: Some(file_count),
        dir_count: Some(dir_count),
    })
}

/// 读取可选 JSON 元数据；数据损坏时作为阻塞错误处理。
fn read_json_file<T>(path: &Path) -> Result<Option<T>>
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
fn write_json_file<T>(path: &Path, value: &T) -> Result<()>
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
fn delete_path(path: &Path) -> Result<()> {
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
fn copy_path(src: &Path, dst: &Path) -> Result<()> {
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

/// 清理 Engine.ini 后折叠连续空行。
fn normalize_blank_lines(lines: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    let mut previous_blank = false;
    for line in lines {
        let blank = line.trim().is_empty();
        if blank && previous_blank {
            continue;
        }
        normalized.push(line.clone());
        previous_blank = blank;
    }
    while normalized
        .first()
        .is_some_and(|line| line.trim().is_empty())
    {
        normalized.remove(0);
    }
    while normalized.last().is_some_and(|line| line.trim().is_empty()) {
        normalized.pop();
    }
    normalized
}

/// 从目录中移除已知子项，并返回已删除路径。
fn remove_known_children(
    base_dir: &Path,
    known_names: &[&str],
    logger: Logger,
) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    if !base_dir.is_dir() {
        return Ok(removed);
    }
    let known: HashSet<&str> = known_names.iter().copied().collect();
    for entry in fs::read_dir(base_dir)? {
        let entry = entry?;
        let child = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !known.contains(name.as_ref()) {
            continue;
        }
        logger(format!(
            "[{}] 清理旧模组残留：{}",
            now_text(),
            child.display()
        ));
        delete_path(&child)?;
        removed.push(child.display().to_string());
    }
    if base_dir.exists() && fs::read_dir(base_dir)?.next().is_none() {
        logger(format!(
            "[{}] 删除空目录：{}",
            now_text(),
            base_dir.display()
        ));
        fs::remove_dir(base_dir)?;
        removed.push(base_dir.display().to_string());
    }
    Ok(removed)
}

/// 清理会和 Rust 工具箱冲突的旧一键包残留。
fn clean_legacy_range_mod(target_win64: &Path, logger: Logger) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    let project_boundary_dir = target_win64
        .parent()
        .and_then(Path::parent)
        .context("无法定位 ProjectBoundary 目录")?;
    let logic_mods_dir = project_boundary_dir
        .join("Content")
        .join("Paks")
        .join("LogicMods");
    for name in OLD_MOD_LOGICMOD_FILES {
        let path = logic_mods_dir.join(name);
        if path.exists() {
            logger(format!(
                "[{}] 清理旧模组残留：{}",
                now_text(),
                path.display()
            ));
            delete_path(&path)?;
            removed.push(path.display().to_string());
        }
    }
    if logic_mods_dir.is_dir() && fs::read_dir(&logic_mods_dir)?.next().is_none() {
        logger(format!(
            "[{}] 删除空目录：{}",
            now_text(),
            logic_mods_dir.display()
        ));
        fs::remove_dir(&logic_mods_dir)?;
        removed.push(logic_mods_dir.display().to_string());
    }

    let root_signature_removed = remove_known_children(
        &target_win64.join("Mods"),
        OLD_MOD_UE4SS_MOD_ENTRIES,
        logger.clone(),
    )?;
    let nested_ue4ss_dir = target_win64.join("ue4ss");
    let nested_signature_removed = remove_known_children(
        &nested_ue4ss_dir.join("Mods"),
        OLD_MOD_UE4SS_MOD_ENTRIES,
        logger.clone(),
    )?;
    let mut nested_known = OLD_MOD_UE4SS_SUPPORT_FILES.to_vec();
    nested_known.extend_from_slice(OLD_MOD_UE4SS_LOADER_FILES);
    let nested_support_removed =
        remove_known_children(&nested_ue4ss_dir, &nested_known, logger.clone())?;

    let root_support_present = OLD_MOD_UE4SS_SUPPORT_FILES
        .iter()
        .any(|name| target_win64.join(name).exists());
    let root_signature_found = !root_signature_removed.is_empty();
    let nested_signature_found =
        !nested_signature_removed.is_empty() || !nested_support_removed.is_empty();

    removed.extend(root_signature_removed);
    if root_signature_found || root_support_present {
        removed.extend(remove_known_children(
            target_win64,
            OLD_MOD_UE4SS_SUPPORT_FILES,
            logger.clone(),
        )?);
    }
    if root_signature_found || root_support_present {
        for name in OLD_MOD_UE4SS_LOADER_FILES {
            let path = target_win64.join(name);
            if path.exists() {
                logger(format!(
                    "[{}] 清理旧模组加载器：{}",
                    now_text(),
                    path.display()
                ));
                delete_path(&path)?;
                removed.push(path.display().to_string());
            }
        }
    }

    if nested_signature_found {
        removed.extend(nested_signature_removed);
        removed.extend(nested_support_removed);
        if nested_ue4ss_dir.is_dir() && fs::read_dir(&nested_ue4ss_dir)?.next().is_none() {
            logger(format!(
                "[{}] 删除空目录：{}",
                now_text(),
                nested_ue4ss_dir.display()
            ));
            fs::remove_dir(&nested_ue4ss_dir)?;
            removed.push(nested_ue4ss_dir.display().to_string());
        }
    }
    Ok(removed)
}

/// 移除会破坏本地社区服登录的旧 OnlineSubsystem 配置。
fn clean_engine_ini(logger: Logger) -> Result<Option<PathBuf>> {
    let local_appdata = env::var("LOCALAPPDATA").context("未找到 LOCALAPPDATA")?;
    let engine_ini = PathBuf::from(local_appdata)
        .join("ProjectBoundary")
        .join("Saved")
        .join("Config")
        .join("WindowsClient")
        .join("Engine.ini");
    if !engine_ini.exists() {
        logger(format!(
            "[{}] Engine.ini 不存在，跳过：{}",
            now_text(),
            engine_ini.display()
        ));
        return Ok(None);
    }

    let content = fs::read_to_string(&engine_ini).unwrap_or_default();
    let lines: Vec<String> = content.lines().map(ToOwned::to_owned).collect();
    let header_re = Regex::new(r"^\[(?P<section>[^\]]+)\]\s*$")?;
    let key_re =
        Regex::new(r"(?i)^DefaultPlatformService\s*=\s*<Default Platform Identifier>\s*$")?;
    let mut output = Vec::new();
    let mut current_section: Option<String> = None;
    let mut section_body: Vec<String> = Vec::new();
    let mut removed = false;

    // 按 section 重建文件，确保无关用户设置和原有顺序在清理后仍保留。
    let mut flush_section =
        |section_name: Option<String>, section_lines: &[String], output_lines: &mut Vec<String>| {
            if section_name.as_deref() != Some("OnlineSubsystem") {
                if let Some(name) = section_name {
                    output_lines.push(format!("[{}]", name));
                }
                output_lines.extend(section_lines.iter().cloned());
                return;
            }

            let mut kept = Vec::new();
            let mut removed_here = false;
            for line in section_lines {
                if key_re.is_match(line.trim()) {
                    removed_here = true;
                    removed = true;
                    continue;
                }
                kept.push(line.clone());
            }

            if kept.iter().any(|line| !line.trim().is_empty()) {
                output_lines.push("[OnlineSubsystem]".to_string());
                output_lines.extend(kept);
                if removed_here {
                    logger(format!(
                        "[{}] 已删除 [OnlineSubsystem] 节内的冲突键值。",
                        now_text()
                    ));
                }
                return;
            }

            if removed_here || !section_lines.is_empty() {
                removed = true;
                logger(format!(
                    "[{}] 已删除 [OnlineSubsystem] 冲突节。",
                    now_text()
                ));
            }
        };

    for line in lines {
        if let Some(captures) = header_re.captures(line.trim()) {
            flush_section(current_section.take(), &section_body, &mut output);
            current_section = Some(captures["section"].to_string());
            section_body.clear();
        } else {
            section_body.push(line);
        }
    }
    flush_section(current_section.take(), &section_body, &mut output);

    let normalized = normalize_blank_lines(&output);
    let mut new_content = normalized.join("\r\n");
    if !new_content.is_empty() {
        new_content.push_str("\r\n");
    }
    if removed && new_content != content {
        fs::write(&engine_ini, new_content)?;
        logger(format!(
            "[{}] Engine.ini 已清理：{}",
            now_text(),
            engine_ini.display()
        ));
        return Ok(Some(engine_ini));
    }
    logger(format!(
        "[{}] Engine.ini 未发现需要清理的冲突配置。",
        now_text()
    ));
    Ok(None)
}

/// 列出当前 Windows 上存在的盘符根目录。
pub fn list_available_drives() -> Vec<PathBuf> {
    ('A'..='Z')
        .map(|letter| PathBuf::from(format!("{}:\\", letter)))
        .filter(|path| path.exists())
        .collect()
}

/// 在单个盘符下深度优先扫描游戏主程序。
fn scan_drive_for_game(drive_root: PathBuf, stop_flag: Arc<AtomicBool>) -> Option<PathBuf> {
    let mut stack = vec![drive_root];
    let skip_names: HashSet<&str> = ["$recycle.bin", "system volume information"]
        .into_iter()
        .collect();
    while let Some(current) = stack.pop() {
        if stop_flag.load(Ordering::Relaxed) {
            return None;
        }
        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        let mut child_dirs = Vec::new();
        for entry in entries.flatten() {
            if stop_flag.load(Ordering::Relaxed) {
                return None;
            }
            let path = entry.path();
            if path.is_file()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(GAME_EXE))
            {
                let candidate = path.parent()?.to_path_buf();
                if validate_win64_path(&candidate).is_ok() {
                    return Some(candidate);
                }
            } else if path.is_dir() {
                let name = entry.file_name().to_string_lossy().to_lowercase();
                // 跳过常见系统目录；这些目录通常会拒绝访问，正常安装也不会包含 Steam 库。
                if skip_names.contains(name.as_str()) {
                    continue;
                }
                child_dirs.push(path);
            }
        }
        child_dirs.reverse();
        stack.extend(child_dirs);
    }
    None
}

/// 并发扫描多个盘符，命中第一个结果后通知其它工作线程停止。
fn scan_drives_for_game(drives: &[PathBuf], logger: Logger) -> Option<PathBuf> {
    if drives.is_empty() {
        return None;
    }
    let stop_flag = Arc::new(AtomicBool::new(false));
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut handles = Vec::new();
    for drive in drives.iter().cloned() {
        logger(format!(
            "[{}] 开始扫描盘符：{}",
            now_text(),
            drive.display()
        ));
        let stop = stop_flag.clone();
        let tx = tx.clone();
        handles.push(thread::spawn(move || {
            let result = scan_drive_for_game(drive.clone(), stop);
            let _ = tx.send((drive, result));
        }));
    }
    drop(tx);

    let mut found = None;
    for (drive, result) in &rx {
        if let Some(path) = result {
            logger(format!(
                "[{}] 扫描命中：{} -> {}",
                now_text(),
                drive.display(),
                path.display()
            ));
            stop_flag.store(true, Ordering::Relaxed);
            found = Some(path);
            break;
        }
        logger(format!(
            "[{}] 扫描完成，未找到游戏目录：{}",
            now_text(),
            drive.display()
        ));
    }

    for handle in handles {
        let _ = handle.join();
    }
    found
}

/// 校验路径是否为 Boundary 的精确 Binaries\Win64 目录。
pub fn validate_win64_path(path: &Path) -> Result<()> {
    if !path.exists() {
        bail!("目录不存在：{}", path.display());
    }
    if !path.is_dir() {
        bail!("不是目录：{}", path.display());
    }
    if !path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("Win64"))
    {
        bail!("目标目录不是 Win64。");
    }
    let exe_path = path.join(GAME_EXE);
    if !exe_path.exists() {
        bail!("未找到游戏主程序：{}", exe_path.display());
    }
    Ok(())
}

/// 将用户选择的各种游戏根目录形式解析为 Binaries\Win64。
pub fn normalize_selected_path(raw_path: &Path) -> Result<PathBuf> {
    let selected = raw_path
        .canonicalize()
        .unwrap_or_else(|_| raw_path.to_path_buf());
    let candidates = [
        selected.clone(),
        selected
            .join("ProjectBoundary")
            .join("Binaries")
            .join("Win64"),
        selected.join("Binaries").join("Win64"),
    ];
    for candidate in candidates {
        if validate_win64_path(&candidate).is_ok() {
            return Ok(candidate);
        }
    }
    bail!(
        "请选择 Boundary 游戏根目录、ProjectBoundary 目录，或 ProjectBoundary\\Binaries\\Win64 目录。"
    )
}

/// 从 HKCU/HKLM 注册表键读取常见 Steam 安装根目录。
fn steam_registry_paths() -> Vec<PathBuf> {
    let mut results = Vec::new();
    let registry_candidates = [
        (HKEY_CURRENT_USER, r"Software\Valve\Steam", "SteamPath"),
        (HKEY_CURRENT_USER, r"Software\Valve\Steam", "SteamExe"),
        (
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\WOW6432Node\Valve\Steam",
            "InstallPath",
        ),
        (HKEY_LOCAL_MACHINE, r"SOFTWARE\Valve\Steam", "InstallPath"),
    ];
    for (root, subkey, value_name) in registry_candidates {
        let key = match RegKey::predef(root).open_subkey(subkey) {
            Ok(key) => key,
            Err(_) => continue,
        };
        let value: String = match key.get_value(value_name) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let mut path = PathBuf::from(value.replace('/', "\\"));
        if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
        {
            path.pop();
        }
        results.push(path);
    }
    results
}

/// 构造 Steam libraryfolders.vdf 的候选路径列表。
fn candidate_libraryfolders_files() -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    for steam_root in steam_registry_paths() {
        let path = steam_root.join("steamapps").join("libraryfolders.vdf");
        let key = path.to_string_lossy().to_lowercase();
        if seen.insert(key) {
            candidates.push(path);
        }
    }
    for path in [
        PathBuf::from(r"C:\Program Files (x86)\Steam\steamapps\libraryfolders.vdf"),
        PathBuf::from(r"C:\Program Files\Steam\steamapps\libraryfolders.vdf"),
    ] {
        let key = path.to_string_lossy().to_lowercase();
        if seen.insert(key) {
            candidates.push(path);
        }
    }
    candidates
}

/// 用小范围正则解析 Steam VDF 中的库路径。
fn parse_library_paths(libraryfolders_path: &Path) -> Result<Vec<PathBuf>> {
    let content = fs::read_to_string(libraryfolders_path)?;
    let regex = Regex::new(r#""path"\s+"([^"]+)""#)?;
    Ok(regex
        .captures_iter(&content)
        .map(|captures| PathBuf::from(captures[1].replace("\\\\", "\\")))
        .collect())
}

/// 当 Boundary appmanifest 存在时读取其安装目录。
fn read_manifest_install_dir(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)?;
    let regex = Regex::new(r#""installdir"\s+"([^"]+)""#)?;
    Ok(regex
        .captures(&content)
        .map(|captures| captures[1].to_string()))
}

/// 根据 Steam 元数据检测 Boundary 的 Win64 路径。
fn detect_steam_game_win64() -> Result<(PathBuf, String)> {
    let mut errors = Vec::new();
    for libraryfolders_path in candidate_libraryfolders_files() {
        if !libraryfolders_path.exists() {
            errors.push(format!(
                "未找到 Steam 库配置：{}",
                libraryfolders_path.display()
            ));
            continue;
        }
        let library_paths = match parse_library_paths(&libraryfolders_path) {
            Ok(paths) => paths,
            Err(error) => {
                errors.push(format!(
                    "读取失败 {}: {}",
                    libraryfolders_path.display(),
                    error
                ));
                continue;
            }
        };
        for library_root in library_paths {
            let manifest = library_root
                .join("steamapps")
                .join(format!("appmanifest_{}.acf", APP_ID));
            let Some(install_dir) = read_manifest_install_dir(&manifest)? else {
                continue;
            };
            let win64_path = library_root
                .join("steamapps")
                .join("common")
                .join(install_dir)
                .join("ProjectBoundary")
                .join("Binaries")
                .join("Win64");
            match validate_win64_path(&win64_path) {
                Ok(()) => {
                    return Ok((
                        win64_path.clone(),
                        format!("已通过 Steam 自动识别：{}", win64_path.display()),
                    ));
                }
                Err(error) => {
                    errors.push(format!("{} 指向的目录无效：{}", manifest.display(), error));
                }
            }
        }
    }
    if errors.is_empty() {
        errors.push(format!(
            "未在 Steam 库中找到 App ID {}（Boundary）。",
            APP_ID
        ));
    }
    bail!(errors.join("\n"))
}

/// 为弹窗和日志生成紧凑的进程摘要字符串。
pub fn summarize_runtime_processes(snapshot: &RuntimeSnapshot) -> String {
    let parts = [
        ("游戏", &snapshot.game),
        ("服务包装器", &snapshot.wrapper),
        ("登录服务器", &snapshot.server),
        ("置顶守护", &snapshot.watcher),
    ]
    .into_iter()
    .map(|(label, items)| {
        if items.is_empty() {
            format!("{label} 0 个")
        } else {
            let details = items
                .iter()
                .map(format_runtime_process)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{label} {} 个（{}）", items.len(), details)
        }
    })
    .collect::<Vec<_>>();
    parts.join("；")
}

/// 格式化单个进程，包含 PID 以及 exe 路径或命令行。
fn format_runtime_process(process: &RuntimeProcess) -> String {
    let name = if process.name.trim().is_empty() {
        "未知进程"
    } else {
        process.name.trim()
    };
    let detail = if !process.exe.trim().is_empty() {
        process.exe.trim()
    } else {
        process.cmd.trim()
    };
    if detail.is_empty() {
        format!("{} PID {}", name, process.pid)
    } else {
        format!(
            "{} PID {} @ {}",
            name,
            process.pid,
            shorten_runtime_detail(detail)
        )
    }
}

/// 缩短长命令行，避免 UI 弹窗难以阅读。
fn shorten_runtime_detail(value: &str) -> String {
    let value = value.trim();
    if value.chars().count() <= 96 {
        return value.to_string();
    }
    let mut shortened = value.chars().take(93).collect::<String>();
    shortened.push_str("...");
    shortened
}

/// 对多个运行时进程组中的 PID 去重。
fn runtime_process_pids(snapshot: &RuntimeSnapshot) -> Vec<u32> {
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
    pids
}

/// 为确认弹窗和错误信息格式化端口占用列表。
pub fn format_port_conflicts(conflicts: &[PortConflict]) -> String {
    conflicts
        .iter()
        .map(|item| {
            format!(
                "- {}/{} -> PID {} {} ({})",
                item.protocol.to_uppercase(),
                item.port,
                item.pid,
                item.name,
                item.exe
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 读取系统 socket 表，并筛选本地服务需要的端口。
fn collect_port_conflicts() -> Result<Vec<PortConflict>> {
    let sockets = get_sockets_info(
        AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6,
        ProtocolFlags::TCP | ProtocolFlags::UDP,
    )?;
    let mut system = System::new_all();
    system.refresh_all();
    let mut conflicts = Vec::new();

    for socket in sockets {
        match socket.protocol_socket_info {
            ProtocolSocketInfo::Tcp(tcp) => {
                if tcp.state != TcpState::Listen || !REQUIRED_TCP_PORTS.contains(&tcp.local_port) {
                    continue;
                }
                append_conflicts(
                    "TCP",
                    tcp.local_port,
                    &socket.associated_pids,
                    &system,
                    &mut conflicts,
                );
            }
            ProtocolSocketInfo::Udp(udp) => {
                if !REQUIRED_UDP_PORTS.contains(&udp.local_port) {
                    continue;
                }
                append_conflicts(
                    "UDP",
                    udp.local_port,
                    &socket.associated_pids,
                    &system,
                    &mut conflicts,
                );
            }
        }
    }

    conflicts.sort_by(|left, right| {
        (left.protocol.as_str(), left.port, left.pid).cmp(&(
            right.protocol.as_str(),
            right.port,
            right.pid,
        ))
    });
    conflicts.dedup_by(|left, right| {
        left.protocol == right.protocol && left.port == right.port && left.pid == right.pid
    });
    Ok(conflicts)
}

/// 追加端口冲突行，并兼容缺少进程元数据的 socket。
fn append_conflicts(
    protocol: &str,
    port: u16,
    pids: &[u32],
    system: &System,
    conflicts: &mut Vec<PortConflict>,
) {
    if pids.is_empty() {
        conflicts.push(PortConflict {
            protocol: protocol.to_string(),
            port,
            pid: 0,
            name: "未知进程".to_string(),
            exe: "未知路径".to_string(),
        });
        return;
    }

    for pid in pids {
        let process = system.process(sysinfo::Pid::from_u32(*pid));
        conflicts.push(PortConflict {
            protocol: protocol.to_string(),
            port,
            pid: *pid,
            name: process
                .map(|process| process.name().to_string_lossy().into_owned())
                .unwrap_or_else(|| "未知进程".to_string()),
            exe: process
                .and_then(|process| process.exe())
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "未知路径".to_string()),
        });
    }
}

/// 结束指定 PID，并把任何 taskkill 失败都作为用户可见错误。
fn kill_pids(pids: &[u32]) -> Result<()> {
    let failures = taskkill_pids(pids)?;
    if !failures.is_empty() {
        bail!("结束进程失败：\n{}", failures.join("\n"));
    }
    Ok(())
}

/// 逐个 PID 执行 taskkill，并返回详细失败信息而不是吞掉错误。
fn taskkill_pids(pids: &[u32]) -> Result<Vec<String>> {
    let mut failures = Vec::new();
    for pid in pids {
        let output = hidden_taskkill_command()
            .arg("/PID")
            .arg(pid.to_string())
            .arg("/T")
            .arg("/F")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("结束 PID {} 失败", pid))?;
        if !output.status.success() {
            failures.push(format!(
                "PID {}：{}",
                pid,
                taskkill_output_text(&output.stdout, &output.stderr, output.status.code())
            ));
        }
    }
    Ok(failures)
}

/// 选择最有用的 taskkill 诊断文本。
fn taskkill_output_text(stdout: &[u8], stderr: &[u8], code: Option<i32>) -> String {
    let stdout = String::from_utf8_lossy(stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        match code {
            Some(code) => format!("taskkill 退出码 {}", code),
            None => "taskkill 被系统终止".to_string(),
        }
    }
}

/// 从所选 Win64 目录推导启动所需的可执行文件路径。
fn launch_files(target_win64: &Path) -> LaunchFiles {
    LaunchFiles {
        server_dir: target_win64.join("BoundaryMetaServer-main"),
        node_exe: target_win64.join("nodejs").join("node.exe"),
        wrapper_exe: target_win64.join("ProjectReboundServerWrapper.exe"),
        game_exe: target_win64.join(GAME_EXE),
    }
}

/// 用于进程匹配的规范化小写路径。
fn path_match_key(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_lowercase()
}
