//! 核心安装、启动、检测和清理逻辑。
//!
//! 这个模块集中负责文件系统变更和进程操作。Slint UI 只调用 `InstallerCore`，
//! 替换文件、恢复备份、结束进程、读取 Steam 元数据、校验在线下载等高风险动作
//! 都收敛在这里处理。

use std::path::PathBuf;
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};

use serde::{Deserialize, Serialize};

/// 显示在 UI 中并写入安装元数据的应用版本。
pub const APP_VERSION: &str = "19.19.83";
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
/// 默认 GitHub 代理前缀；留空则直接访问 GitHub。
pub const DEFAULT_GITHUB_PROXY_PREFIX: &str = "https://git-proxy.cubland.icu/";
/// 用于两个 ProjectRebound 运行时二进制的 Nightly 包原始地址。
pub const PROJECT_REBOUND_RELEASE_URL: &str =
    "https://github.com/STanJK/ProjectRebound/releases/download/Nightly/Release.zip";
/// 这些文件刻意从线上获取，而不是放进 payload.zip。
pub const PROJECT_REBOUND_ONLINE_FILES: &[&str] =
    &["Payload.dll", "ProjectReboundServerWrapper.exe"];
/// 登录服务器源码包原始地址。
pub const BOUNDARY_META_SERVER_ARCHIVE_URL: &str =
    "https://github.com/STanJK/BoundaryMetaServer/archive/refs/heads/main.zip";
/// 安装到目标 Win64 目录内的登录服务器目录名。
pub const BOUNDARY_META_SERVER_DIR_NAME: &str = "BoundaryMetaServer-main";
/// Node.js 官方版本索引，用于安装本地登录服务器运行时。
pub const NODEJS_DIST_INDEX_URL: &str = "https://nodejs.org/dist/index.json";
/// 安装到目标 Win64 目录内的 Node.js 运行时目录名。
pub const NODEJS_DIR_NAME: &str = "nodejs";
/// 安装 BoundaryMetaServer 依赖时使用的国内 npm 镜像源。
pub const NPM_REGISTRY_URL: &str = "https://registry.npmmirror.com";
/// Wintun 官方下载包。该源不在 GitHub，不需要 git-proxy。
pub const WINTUN_RELEASE_URL: &str = "https://www.wintun.net/builds/wintun-0.14.1.zip";
pub const WINTUN_RELEASE_NAME: &str = "wintun-0.14.1.zip";
/// 本地登录/游戏服务需要的 TCP 端口。
pub const REQUIRED_TCP_PORTS: &[u16] = &[6969, 7777, 8000, 9000];
/// 本地游戏服务需要的 UDP 端口。
pub const REQUIRED_UDP_PORTS: &[u16] = &[7777, 9000];
/// 游戏连接本地登录服务器所用的启动参数值。
pub const LOCAL_LOGIC_SERVER_URL: &str = "http://127.0.0.1:8000";
/// 工具箱 UI 首选字体。
pub const UI_FONT_FAMILY: &str = "Maple Mono NF CN";
/// Maple Mono 官方 GitHub 最新 Release API。
pub const MAPLE_FONT_LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/subframe7536/maple-font/releases/latest";
/// 优先下载带 Nerd Font 和中文补全的非 hinted 包。
pub const MAPLE_FONT_RELEASE_ASSET: &str = "MapleMono-NF-CN-unhinted.zip";
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
        name: "nodejs",
        kind: ItemKind::Dir,
    },
    ManagedItem {
        name: BOUNDARY_META_SERVER_DIR_NAME,
        kind: ItemKind::Dir,
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
pub type ProgressReporter = Arc<dyn Fn(InstallProgress) + Send + Sync + 'static>;

#[derive(Debug, Clone, Default)]
pub struct InstallCancelToken {
    cancelled: Arc<AtomicBool>,
}

impl InstallCancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

/// 安装过程的可见进度。value 使用 0.0..=1.0，detail 放当前下载/解压细节。
#[derive(Debug, Clone)]
pub struct InstallProgress {
    pub value: f32,
    pub title: String,
    pub detail: String,
}

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

/// 用户选择的启动模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    Pvp,
    Pve,
}

impl LaunchMode {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Pvp => "PVP",
            Self::Pve => "PVE",
        }
    }

    pub fn uses_login_server(self) -> bool {
        matches!(self, Self::Pvp | Self::Pve)
    }
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
    github_proxy_prefix: Arc<RwLock<String>>,
}

pub(crate) mod cleanup;
pub(crate) mod discovery;
pub(crate) mod filesystem;
pub(crate) mod font;
pub(crate) mod install_ops;
pub(crate) mod installer;
pub(crate) mod metadata;
pub(crate) mod pathing;
pub(crate) mod payload;
pub(crate) mod process;
pub(crate) mod runtime_ops;
pub(crate) mod util;

pub use installer::normalize_github_proxy_prefix;
pub use process::format_port_conflicts;
pub use util::{is_running_as_administrator, now_text};
