//! Slint 应用控制器、UI 模型和后台任务调度。

use crate::core;
use crate::vnt_platform;
use crate::{AppWindow, DriveRow, PortRow, ServerRow, VntPeerRow};

use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Result, bail};
use crossbeam_channel::{Receiver, Sender, unbounded};
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::core::{
    APP_VERSION, DEFAULT_KEEP_TOPMOST, DEFAULT_TOPMOST_HOTKEY, InstallerCore, MONITORED_PORTS,
    PathMode, PortConflict, PortStatusRow as CorePortStatusRow, format_port_conflicts,
};
use crate::vnt_platform::{VntEvent, VntLaunchOptions, VntSession};

mod background;
mod controller;
mod diagnostics;
mod hotkey;
mod server_list;
mod vnt_rows;

use background::spawn_port_thread;
use diagnostics::{format_process_detection_message, runtime_snapshot_has_any};
use hotkey::{hotkey_capture_is_escape, hotkey_from_capture};
use server_list::{RemoteServer, fetch_servers, server_placeholder_row, server_to_row};
use vnt_rows::{apply_vnt_idle_to_ui, vnt_peer_to_row, vnt_placeholder_rows};

pub(crate) fn run() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    // 隐藏子进程使用该模式维持游戏窗口置顶。
    if let Some(result) = core::watch_mode_from_args(&args) {
        std::process::exit(result?);
    }

    let app = AppWindow::new()?;
    let controller = AppController::new(app)?;
    AppController::bind_callbacks(&controller);
    AppController::start_background_timers(&controller);
    controller.borrow_mut().initialize();
    let ui = controller.borrow().ui.as_weak().unwrap();
    let stop_background = controller.borrow().stop_background.clone();
    ui.run()?;
    stop_background.store(true, Ordering::Relaxed);
    controller.borrow_mut().shutdown();
    Ok(())
}

/// 工作线程发送回 Slint 主线程的消息。
#[derive(Debug, Clone)]
enum AppMessage {
    Log(String),
    PortRows(Vec<CorePortStatusRow>),
    ServerRows(Vec<RemoteServer>),
    ServerRowsFailed(String),
    VntEvent(VntEvent),
    ActionFinished {
        title: String,
        status: String,
        dialog: String,
        process_status: Option<String>,
        target: Option<PathBuf>,
        load_topmost: bool,
    },
    ActionFailed {
        title: String,
        status: String,
        error: String,
    },
    ScanFinished {
        result: Option<PathBuf>,
        dialog: String,
    },
}

/// 自定义应用内弹窗背后等待执行的动作。
enum PendingDialogAction {
    None,
    LaunchWithConflicts {
        target: PathBuf,
        keep_topmost: bool,
        hotkey: String,
        conflicts: Vec<PortConflict>,
    },
    ManualPathInput,
}

/// 供 Slint 回调共享的可变应用状态。
struct AppController {
    ui: AppWindow,
    core: Arc<InstallerCore>,
    tx: Sender<AppMessage>,
    rx: Receiver<AppMessage>,
    stop_background: Arc<AtomicBool>,
    session_log_file: File,
    mode: PathMode,
    current_target: Option<PathBuf>,
    drive_model: Rc<VecModel<DriveRow>>,
    port_model: Rc<VecModel<PortRow>>,
    server_model: Rc<VecModel<ServerRow>>,
    vnt_peer_model: Rc<VecModel<VntPeerRow>>,
    vnt_session: Option<VntSession>,
    pending_dialog_action: PendingDialogAction,
}
