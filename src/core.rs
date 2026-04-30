//! 核心安装、启动、检测和清理逻辑。
//!
//! 这个模块集中负责文件系统变更和进程操作。Slint UI 只调用 `InstallerCore`，
//! 替换文件、恢复备份、结束进程、读取 Steam 元数据、校验在线下载等高风险动作
//! 都收敛在这里处理。

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

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
}

/// 绑定了必要本地端口的进程。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortConflict {
    pub protocol: String,
    pub port: u16,
    pub pid: u32,
    pub name: String,
    pub exe: String,
    /// 该端口占用是否来自当前目标目录下的预期游戏/服务进程。
    #[serde(default)]
    pub expected: bool,
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

pub(crate) mod cleanup;
pub(crate) mod discovery;
pub(crate) mod filesystem;
pub(crate) mod install_ops;
pub(crate) mod installer;
pub(crate) mod metadata;
pub(crate) mod pathing;
pub(crate) mod payload;
pub(crate) mod process;
pub(crate) mod runtime_ops;
pub(crate) mod util;

pub use process::{format_port_conflicts, summarize_runtime_processes};
pub use util::now_text;
