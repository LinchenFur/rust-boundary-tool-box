#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Slint UI entry point and application controller.
//!
//! `core` owns dangerous filesystem/process operations. This file binds Slint
//! callbacks, maintains UI models, starts background work on threads, and routes
//! results back to the main thread through a channel.

mod core;
mod vnt_platform;
mod win;

use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use crossbeam_channel::{Receiver, Sender, unbounded};
use serde::Deserialize;
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::core::{
    APP_VERSION, DEFAULT_KEEP_TOPMOST, DEFAULT_TOPMOST_HOTKEY, InstallerCore, MONITORED_PORTS,
    PathMode, PortConflict, PortStatusRow as CorePortStatusRow, RuntimeSnapshot,
    format_port_conflicts,
};
use crate::vnt_platform::{VntEvent, VntLaunchOptions, VntPeer, VntSession};

slint::include_modules!();

// Remote community server list endpoint. The request is intentionally tiny and
// implemented with TcpStream so the UI does not need an async HTTP runtime.
const SERVER_LIST_HOST: &str = "ax48735790k.vicp.fun";
const SERVER_LIST_PORT: u16 = 3000;
const SERVER_LIST_PATH: &str = "/servers";

/// One row returned by the community server JSON endpoint.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RemoteServer {
    name: String,
    region: String,
    mode: String,
    map: String,
    port: u16,
    player_count: u32,
    server_state: String,
    ip: String,
    last_heartbeat: i64,
}

fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    // A hidden child process uses this mode to keep the game window topmost.
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

/// Messages sent from worker threads back into the Slint main thread.
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

/// Action waiting behind the custom in-app modal.
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

/// Mutable application state shared by Slint callbacks.
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

impl AppController {
    /// Creates models, log files, core services, and initial UI state.
    fn new(ui: AppWindow) -> Result<Rc<RefCell<Self>>> {
        let (tx, rx) = unbounded();
        // Core logs may be emitted from worker threads, so route them through
        // the same UI-safe channel as other background results.
        let log_tx = tx.clone();
        let logger = Arc::new(move |message: String| {
            let _ = log_tx.send(AppMessage::Log(message));
        });

        let core = Arc::new(InstallerCore::new(logger)?);
        let logs_dir = core.installer_home.join("logs");
        fs::create_dir_all(&logs_dir)?;
        let session_log_path = logs_dir.join(format!(
            "installer_{}.log",
            chrono::Local::now().format("%Y%m%d_%H%M%S")
        ));
        let mut session_log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&session_log_path)?;
        writeln!(
            session_log_file,
            "[{}] 运行目录：{}",
            core::now_text(),
            core.runtime_dir.display()
        )?;
        writeln!(
            session_log_file,
            "[{}] 载荷目录：{}",
            core::now_text(),
            core.payload_label()
        )?;
        writeln!(
            session_log_file,
            "[{}] 安装器目录：{}",
            core::now_text(),
            core.installer_home.display()
        )?;
        writeln!(
            session_log_file,
            "[{}] 会话日志：{}",
            core::now_text(),
            session_log_path.display()
        )?;

        // Slint models are created once and mutated in place to avoid breaking
        // existing ListView bindings.
        let port_model = Rc::new(VecModel::from(
            MONITORED_PORTS
                .iter()
                .map(|(protocol, port)| PortRow {
                    label: SharedString::from(format!("{}/{}", protocol, port)),
                    detail: SharedString::from("检测中..."),
                    occupied: false,
                })
                .collect::<Vec<_>>(),
        ));
        let drive_model = Rc::new(VecModel::<DriveRow>::default());
        let server_model = Rc::new(VecModel::from(vec![server_placeholder_row(
            "正在加载服务器列表",
            "等待接口返回数据",
        )]));
        let vnt_peer_model = Rc::new(VecModel::from(vnt_placeholder_rows()));
        ui.set_port_rows(ModelRc::from(port_model.clone()));
        ui.set_drive_rows(ModelRc::from(drive_model.clone()));
        ui.set_server_rows(ModelRc::from(server_model.clone()));
        ui.set_vnt_peer_rows(ModelRc::from(vnt_peer_model.clone()));
        ui.set_payload_label(core.payload_label().into());
        ui.set_detected_text("正在检测 Steam 安装目录...".into());
        ui.set_target_text("未解析到有效的安装目录".into());
        ui.set_keep_topmost(DEFAULT_KEEP_TOPMOST);
        ui.set_hotkey_text(DEFAULT_TOPMOST_HOTKEY.into());
        ui.set_status_text(format!("准备就绪 / v{}", APP_VERSION).into());
        ui.set_process_status_text("运行进程：未检测".into());
        ui.set_show_logs(false);
        ui.set_busy(false);
        ui.set_pulse(false);
        ui.set_hotkey_listening(false);
        ui.set_auto_mode(true);
        ui.set_has_target(false);
        ui.set_servers_loading(false);
        ui.set_server_status_text("服务器列表：未刷新".into());
        ui.set_show_drive_dialog(false);
        ui.set_show_app_dialog(false);
        ui.set_app_dialog_confirm(false);
        ui.set_app_dialog_input(false);
        ui.set_app_dialog_error(false);
        ui.set_app_dialog_title("".into());
        ui.set_app_dialog_text("".into());
        ui.set_app_dialog_input_text("".into());
        ui.set_app_dialog_primary_text("确定".into());
        ui.set_app_dialog_secondary_text("取消".into());
        ui.set_vnt_server_text("101.35.230.139:6660".into());
        ui.set_vnt_network_code("".into());
        ui.set_vnt_password("".into());
        ui.set_vnt_no_tun(false);
        ui.set_vnt_compress(false);
        ui.set_vnt_rtx(false);
        ui.set_vnt_busy(false);
        ui.set_vnt_running(false);
        apply_vnt_idle_to_ui(&ui);

        let controller = Rc::new(RefCell::new(Self {
            ui,
            core,
            tx,
            rx,
            stop_background: Arc::new(AtomicBool::new(false)),
            session_log_file,
            mode: PathMode::Auto,
            current_target: None,
            drive_model,
            port_model,
            server_model,
            vnt_peer_model,
            vnt_session: None,
            pending_dialog_action: PendingDialogAction::None,
        }));

        // Port monitoring starts immediately; target-specific checks still run
        // only when the user chooses diagnostic actions.
        spawn_port_thread(
            controller.borrow().core.clone(),
            controller.borrow().tx.clone(),
            controller.borrow().stop_background.clone(),
        );
        Ok(controller)
    }

    /// Wires every Slint callback to the controller.
    fn bind_callbacks(controller: &Rc<RefCell<Self>>) {
        let ui = controller.borrow().ui.as_weak();

        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_auto_mode_clicked(move || {
                controller.borrow_mut().set_mode(PathMode::Auto);
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_manual_mode_clicked(move || {
                controller.borrow_mut().set_mode(PathMode::Manual);
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_manual_path_changed(move |text| {
                controller
                    .borrow_mut()
                    .on_manual_path_changed(text.to_string());
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_browse_clicked(move || {
                controller.borrow_mut().browse_path();
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_redetect_clicked(move || {
                controller.borrow_mut().refresh_target_from_mode(false);
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_scan_clicked(move || {
                controller.borrow_mut().open_drive_dialog();
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_keep_topmost_toggled(move |value| {
                controller.borrow().ui.set_keep_topmost(value);
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_hotkey_text_changed(move |text| {
                controller.borrow().ui.set_hotkey_text(text);
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap()
                .on_hotkey_captured(move |text, control, alt, shift, meta| {
                    controller.borrow_mut().capture_hotkey(
                        text.to_string(),
                        control,
                        alt,
                        shift,
                        meta,
                    );
                });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_install_clicked(move || {
                controller.borrow_mut().start_install();
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_uninstall_clicked(move || {
                controller.borrow_mut().start_uninstall();
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_launch_clicked(move || {
                controller.borrow_mut().start_launch();
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_detect_processes_clicked(move || {
                controller.borrow_mut().start_detect_processes();
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_stop_processes_clicked(move || {
                controller.borrow_mut().start_stop_processes();
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_open_logs_clicked(move || {
                controller.borrow().open_logs_dir();
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_refresh_servers_clicked(move || {
                controller.borrow_mut().start_refresh_servers();
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_vnt_start_clicked(move || {
                controller.borrow_mut().start_vnt();
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_vnt_stop_clicked(move || {
                controller.borrow_mut().stop_vnt();
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_vnt_refresh_clicked(move || {
                controller.borrow_mut().refresh_vnt_status_hint();
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_toggle_logs_clicked(move || {
                let current = controller.borrow().ui.get_show_logs();
                controller.borrow().ui.set_show_logs(!current);
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_drive_toggled(move |index, checked| {
                controller.borrow_mut().toggle_drive(index, checked);
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_drive_confirmed(move || {
                controller.borrow_mut().start_drive_scan();
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_drive_cancelled(move || {
                controller.borrow().ui.set_show_drive_dialog(false);
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_app_dialog_primary(move || {
                controller.borrow_mut().handle_dialog_primary();
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_app_dialog_secondary(move || {
                controller.borrow_mut().handle_dialog_secondary();
            });
        }
    }

    /// Starts UI-thread timers for channel draining and small busy animations.
    fn start_background_timers(controller: &Rc<RefCell<Self>>) {
        let log_controller = Rc::clone(controller);
        let log_timer = Timer::default();
        // Slint UI objects are not Send, so worker threads only enqueue
        // messages; this timer applies them on the UI thread.
        log_timer.start(TimerMode::Repeated, Duration::from_millis(100), move || {
            log_controller.borrow_mut().drain_messages();
        });
        std::mem::forget(log_timer);

        let pulse_controller = Rc::clone(controller);
        let pulse_timer = Timer::default();
        pulse_timer.start(TimerMode::Repeated, Duration::from_millis(650), move || {
            let ui = &pulse_controller.borrow().ui;
            if ui.get_busy() {
                ui.set_pulse(!ui.get_pulse());
            } else if ui.get_pulse() {
                ui.set_pulse(false);
            }
        });
        std::mem::forget(pulse_timer);
    }

    /// Performs initial auto-detection and server-list refresh.
    fn initialize(&mut self) {
        self.refresh_target_from_mode(true);
        self.start_refresh_servers();
    }

    /// Adds a line to both the session log file and the visible log panel.
    fn append_log(&mut self, message: &str) {
        let _ = writeln!(self.session_log_file, "{}", message);
        let current = self.ui.get_log_text().to_string();
        let next = if current.is_empty() {
            message.to_string()
        } else {
            format!("{current}\n{message}")
        };
        self.ui.set_log_text(next.into());
    }

    /// Applies queued worker messages to UI state.
    fn drain_messages(&mut self) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                AppMessage::Log(line) => self.append_log(&line),
                AppMessage::PortRows(rows) => self.update_port_rows(rows),
                AppMessage::ServerRows(rows) => {
                    let count = rows.len();
                    self.ui.set_servers_loading(false);
                    self.update_server_rows(rows);
                    self.ui
                        .set_server_status_text(format!("服务器列表：已加载 {count} 个").into());
                    self.append_log(&format!(
                        "[{}] 已刷新服务器列表：{} 个",
                        core::now_text(),
                        count
                    ));
                }
                AppMessage::ServerRowsFailed(error) => {
                    self.ui.set_servers_loading(false);
                    self.ui
                        .set_server_status_text("服务器列表：刷新失败".into());
                    self.set_server_rows(vec![server_placeholder_row(
                        "服务器列表刷新失败",
                        &error,
                    )]);
                    self.append_log(&format!(
                        "[{}] 服务器列表刷新失败：{}",
                        core::now_text(),
                        error
                    ));
                }
                AppMessage::VntEvent(event) => self.apply_vnt_event(event),
                AppMessage::ActionFinished {
                    title,
                    status,
                    dialog,
                    process_status,
                    target,
                    load_topmost,
                } => {
                    self.ui.set_busy(false);
                    self.ui.set_status_text(status.into());
                    if let Some(process_status) = process_status {
                        self.ui.set_process_status_text(process_status.into());
                    }
                    if let Some(target) = target {
                        self.set_current_target(Some(target), "已就绪", load_topmost);
                    } else {
                        self.sync_has_target();
                    }
                    if title == "安装" {
                        // Installation success is intentionally not shown as a
                        // modal because the user asked to avoid that pop-up.
                        self.append_log(&format!(
                            "[{}] 安装完成：{}",
                            core::now_text(),
                            dialog.replace('\n', "；")
                        ));
                    } else {
                        self.show_info_dialog(&title, &dialog);
                    }
                }
                AppMessage::ActionFailed {
                    title,
                    status,
                    error,
                } => {
                    self.ui.set_busy(false);
                    self.ui.set_status_text(status.into());
                    self.sync_has_target();
                    self.show_error_dialog(&title, &error);
                }
                AppMessage::ScanFinished { result, dialog } => {
                    self.ui.set_busy(false);
                    if let Some(path) = result {
                        self.mode = PathMode::Manual;
                        self.ui.set_auto_mode(false);
                        self.ui.set_manual_path(path.display().to_string().into());
                        self.ui.set_detected_text(dialog.clone().into());
                        self.set_current_target(Some(path), "已就绪", true);
                        self.ui.set_status_text("已找到游戏目录".into());
                    } else {
                        self.ui.set_status_text("未找到游戏目录".into());
                        self.sync_has_target();
                    }
                    self.ui.set_show_drive_dialog(false);
                    self.show_info_dialog("全盘扫描", &dialog);
                }
            }
        }
    }

    /// Converts successful server responses into ListView rows.
    fn update_server_rows(&mut self, servers: Vec<RemoteServer>) {
        if servers.is_empty() {
            self.set_server_rows(vec![server_placeholder_row(
                "暂无服务器",
                "接口返回了空列表",
            )]);
            return;
        }

        let rows = servers.into_iter().map(server_to_row).collect::<Vec<_>>();
        self.set_server_rows(rows);
    }

    /// Reconciles the server model without replacing the model object.
    fn set_server_rows(&mut self, rows: Vec<ServerRow>) {
        while self.server_model.row_count() > rows.len() {
            let _ = self.server_model.remove(self.server_model.row_count() - 1);
        }
        for (index, row) in rows.into_iter().enumerate() {
            if index < self.server_model.row_count() {
                self.server_model.set_row_data(index, row);
            } else {
                self.server_model.push(row);
            }
        }
    }

    /// Maps core port diagnostics into Slint rows.
    fn update_port_rows(&mut self, rows: Vec<CorePortStatusRow>) {
        let mapped = rows
            .into_iter()
            .map(|row| PortRow {
                occupied: row.conflict.is_some(),
                label: format!("{}/{}", row.protocol, row.port).into(),
                detail: row
                    .conflict
                    .map(|conflict| format!("占用中：PID {} {}", conflict.pid, conflict.name))
                    .unwrap_or_else(|| "空闲".to_string())
                    .into(),
            })
            .collect::<Vec<_>>();

        while self.port_model.row_count() > mapped.len() {
            let _ = self.port_model.remove(self.port_model.row_count() - 1);
        }
        for (index, row) in mapped.into_iter().enumerate() {
            if index < self.port_model.row_count() {
                self.port_model.set_row_data(index, row);
            } else {
                self.port_model.push(row);
            }
        }
    }

    /// Switches between Steam auto-detection and manual path mode.
    fn set_mode(&mut self, mode: PathMode) {
        self.mode = mode;
        self.ui.set_auto_mode(matches!(mode, PathMode::Auto));
        self.refresh_target_from_mode(false);
    }

    /// Updates manual path state as the user edits the input field.
    fn on_manual_path_changed(&mut self, text: String) {
        self.ui.set_manual_path(text.clone().into());
        if matches!(self.mode, PathMode::Manual) {
            self.refresh_target_from_mode(false);
        }
    }

    /// Opens the custom path input modal instead of a native folder picker.
    fn browse_path(&mut self) {
        if self.ui.get_busy() {
            return;
        }
        self.mode = PathMode::Manual;
        self.ui.set_auto_mode(false);
        let initial = if !self.ui.get_manual_path().is_empty() {
            self.ui.get_manual_path().to_string()
        } else {
            self.current_target
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default()
        };
        self.show_path_dialog(&initial);
    }

    /// Refreshes the current target based on the selected path mode.
    fn refresh_target_from_mode(&mut self, initial: bool) {
        match self.mode {
            PathMode::Auto => match self.core.detect_steam_game_win64() {
                Ok((path, message)) => {
                    self.ui.set_detected_text(message.clone().into());
                    self.set_current_target(Some(path), "已就绪", true);
                    if !initial {
                        self.append_log(&format!("[{}] {}", core::now_text(), message));
                    }
                }
                Err(error) => {
                    let text = format!("自动识别失败：{}", error);
                    self.ui.set_detected_text(text.clone().into());
                    self.ui
                        .set_status_text("可手动选择路径或使用全盘扫描".into());
                    self.set_current_target(None, "可手动选择路径或使用全盘扫描", false);
                    if !initial {
                        self.append_log(&format!("[{}] 自动识别失败：{}", core::now_text(), error));
                    }
                }
            },
            PathMode::Manual => {
                let raw = self.ui.get_manual_path().to_string();
                if raw.trim().is_empty() {
                    self.set_current_target(None, "请选择游戏路径或使用全盘扫描", false);
                    return;
                }
                match self.core.normalize_selected_path(Path::new(raw.trim())) {
                    Ok(path) => {
                        self.set_current_target(Some(path.clone()), "已就绪", true);
                        self.append_log(&format!(
                            "[{}] 手动路径已解析：{}",
                            core::now_text(),
                            path.display()
                        ));
                    }
                    Err(error) => {
                        self.ui.set_status_text(error.to_string().into());
                        self.set_current_target(None, &error.to_string(), false);
                        self.append_log(&format!("[{}] 手动路径无效：{}", core::now_text(), error));
                    }
                }
            }
        }
    }

    /// Stores the current target and mirrors it into Slint properties.
    fn set_current_target(&mut self, path: Option<PathBuf>, status: &str, load_topmost: bool) {
        self.current_target = path;
        self.ui.set_target_text(
            self.current_target
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "未解析到有效的安装目录".to_string())
                .into(),
        );
        self.ui.set_status_text(status.into());
        if self.current_target.is_none() {
            self.ui.set_process_status_text("运行进程：未检测".into());
        }
        if load_topmost && let Some(target) = &self.current_target {
            let config = self.core.read_topmost_config(target);
            self.ui.set_keep_topmost(config.keep_topmost);
            self.ui.set_hotkey_text(config.hotkey.into());
        }
        self.sync_has_target();
    }

    /// Keeps the UI's enable/disable bindings in sync with target availability.
    fn sync_has_target(&self) {
        self.ui.set_has_target(self.current_target.is_some());
    }

    /// Converts Slint key events into a normalized global-hotkey string.
    fn capture_hotkey(&mut self, text: String, control: bool, alt: bool, shift: bool, meta: bool) {
        if hotkey_capture_is_escape(&text) {
            self.ui.set_hotkey_listening(false);
            self.ui.set_status_text("快捷键设置已取消".into());
            return;
        }

        let Some(candidate) = hotkey_from_capture(&text, control, alt, shift, meta) else {
            self.ui.set_status_text("请继续按下主键".into());
            return;
        };

        match core::normalize_hotkey(&candidate) {
            Ok(normalized) => {
                self.ui.set_hotkey_text(normalized.clone().into());
                self.ui.set_hotkey_listening(false);
                self.ui
                    .set_status_text(format!("快捷键已设置：{normalized}").into());
            }
            Err(error) => {
                self.ui
                    .set_status_text(format!("快捷键无效：{}", error).into());
            }
        }
    }

    /// Returns a validated target or a user-facing error for the current mode.
    fn require_target(&mut self) -> Result<PathBuf> {
        match self.mode {
            PathMode::Auto => {
                let (path, _) = self.core.detect_steam_game_win64()?;
                self.set_current_target(Some(path.clone()), "已就绪", false);
                Ok(path)
            }
            PathMode::Manual => {
                let raw = self.ui.get_manual_path().to_string();
                if raw.trim().is_empty() {
                    bail!("请先选择游戏路径。");
                }
                let path = self.core.normalize_selected_path(Path::new(raw.trim()))?;
                self.set_current_target(Some(path.clone()), "已就绪", false);
                Ok(path)
            }
        }
    }

    /// Opens the custom drive-selection modal for full-drive scans.
    fn open_drive_dialog(&mut self) {
        if self.ui.get_busy() {
            return;
        }
        let drives = self.core.list_available_drives();
        if drives.is_empty() {
            self.show_error_dialog("全盘扫描", "未找到可扫描的盘符。");
            return;
        }

        while self.drive_model.row_count() > 0 {
            let _ = self.drive_model.remove(self.drive_model.row_count() - 1);
        }
        for drive in drives {
            self.drive_model.push(DriveRow {
                label: SharedString::from(drive.display().to_string()),
                checked: true,
            });
        }
        self.ui.set_show_drive_dialog(true);
    }

    /// Updates one drive row after the user toggles it.
    fn toggle_drive(&mut self, index: i32, checked: bool) {
        let index = index.max(0) as usize;
        if let Some(mut row) = self.drive_model.row_data(index) {
            row.checked = checked;
            self.drive_model.set_row_data(index, row);
        }
    }

    /// Collects the currently checked drive roots.
    fn selected_drives(&self) -> Vec<PathBuf> {
        (0..self.drive_model.row_count())
            .filter_map(|index| self.drive_model.row_data(index))
            .filter(|row| row.checked)
            .map(|row| PathBuf::from(row.label.to_string()))
            .collect()
    }

    /// Starts drive scanning on a worker thread.
    fn start_drive_scan(&mut self) {
        let drives = self.selected_drives();
        if drives.is_empty() {
            self.show_error_dialog("全盘扫描", "请至少选择一个盘符。");
            return;
        }
        if self.ui.get_busy() {
            return;
        }

        self.ui.set_busy(true);
        self.ui.set_status_text("全盘扫描中...".into());
        self.append_log(&format!(
            "[{}] 开始全盘扫描：{}",
            core::now_text(),
            drives
                .iter()
                .map(|drive| drive.display().to_string())
                .collect::<Vec<_>>()
                .join("、")
        ));

        let core = self.core.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = core.scan_drives_for_game(&drives);
            let dialog = result
                .as_ref()
                .map(|path| format!("已通过全盘扫描找到游戏目录：{}", path.display()))
                .unwrap_or_else(|| "在所选盘符中未找到 Boundary 游戏目录。".to_string());
            let _ = tx.send(AppMessage::ScanFinished { result, dialog });
        });
    }

    /// Fetches the remote community server list on a worker thread.
    fn start_refresh_servers(&mut self) {
        if self.ui.get_servers_loading() {
            return;
        }

        self.ui.set_servers_loading(true);
        self.ui
            .set_server_status_text("服务器列表：刷新中...".into());
        let tx = self.tx.clone();
        thread::spawn(move || match fetch_servers() {
            Ok(rows) => {
                let _ = tx.send(AppMessage::ServerRows(rows));
            }
            Err(error) => {
                let _ = tx.send(AppMessage::ServerRowsFailed(error.to_string()));
            }
        });
    }

    /// Starts the vendored VNT core and streams its status into the UI.
    fn start_vnt(&mut self) {
        if self.vnt_session.is_some() || self.ui.get_vnt_busy() {
            return;
        }

        let options = VntLaunchOptions {
            server_text: self.ui.get_vnt_server_text().to_string(),
            network_code: self.ui.get_vnt_network_code().to_string(),
            password: self.ui.get_vnt_password().to_string(),
            no_tun: self.ui.get_vnt_no_tun(),
            compress: self.ui.get_vnt_compress(),
            rtx: self.ui.get_vnt_rtx(),
            no_punch: false,
        };
        let tx = self.tx.clone();
        let sink = Arc::new(move |event| {
            let _ = tx.send(AppMessage::VntEvent(event));
        });

        self.ui.set_vnt_busy(true);
        self.ui.set_vnt_status_text("启动中".into());
        self.ui.set_vnt_detail_text("正在启动联机平台".into());
        self.append_log(&format!(
            "[{}] 启动 VNT 联机：{} / {}",
            core::now_text(),
            options.server_text,
            options.network_code
        ));

        match VntSession::start(options, sink) {
            Ok(session) => {
                self.vnt_session = Some(session);
            }
            Err(error) => {
                self.ui.set_vnt_busy(false);
                self.ui.set_vnt_running(false);
                self.ui.set_vnt_status_text("启动失败".into());
                self.ui.set_vnt_detail_text(error.to_string().into());
                self.append_log(&format!("[{}] VNT 启动失败：{}", core::now_text(), error));
                self.show_error_dialog("联机", &error.to_string());
            }
        }
    }

    /// Requests the VNT session to stop; final cleanup arrives as an event.
    fn stop_vnt(&mut self) {
        if let Some(session) = self.vnt_session.as_mut() {
            session.stop();
            self.ui.set_vnt_busy(true);
            self.ui.set_vnt_status_text("停止中".into());
            self.ui.set_vnt_detail_text("正在关闭联机平台".into());
            self.append_log(&format!("[{}] 正在停止 VNT 联机", core::now_text()));
        } else {
            self.apply_vnt_snapshot(vnt_platform::idle_snapshot());
        }
    }

    /// Gives immediate feedback while waiting for the next VNT snapshot tick.
    fn refresh_vnt_status_hint(&mut self) {
        if self.vnt_session.is_some() {
            self.ui.set_vnt_detail_text("等待联机核心刷新状态".into());
        }
    }

    /// Applies lifecycle events emitted by the VNT worker thread.
    fn apply_vnt_event(&mut self, event: VntEvent) {
        match event {
            VntEvent::Snapshot(snapshot) => self.apply_vnt_snapshot(snapshot),
            VntEvent::Failed(error) => {
                self.vnt_session = None;
                self.ui.set_vnt_busy(false);
                self.ui.set_vnt_running(false);
                self.ui.set_vnt_status_text("启动失败".into());
                self.ui.set_vnt_detail_text(error.clone().into());
                self.set_vnt_peer_rows(vnt_placeholder_rows());
                self.append_log(&format!("[{}] VNT 异常：{}", core::now_text(), error));
                self.show_error_dialog("联机", &error);
            }
            VntEvent::Stopped(reason) => {
                self.vnt_session = None;
                let mut snapshot = vnt_platform::idle_snapshot();
                snapshot.detail = reason.clone();
                self.apply_vnt_snapshot(snapshot);
                self.append_log(&format!("[{}] VNT 已停止：{}", core::now_text(), reason));
            }
        }
    }

    /// Mirrors a VNT snapshot into the Slint properties and peer model.
    fn apply_vnt_snapshot(&mut self, snapshot: vnt_platform::VntSnapshot) {
        self.ui.set_vnt_running(snapshot.running);
        self.ui.set_vnt_busy(snapshot.busy);
        self.ui.set_vnt_status_text(snapshot.status.into());
        self.ui.set_vnt_detail_text(snapshot.detail.into());
        self.ui.set_vnt_ip_text(snapshot.virtual_ip.into());
        self.ui.set_vnt_server_status_text(snapshot.server.into());
        self.ui.set_vnt_nat_text(snapshot.nat.into());
        self.ui
            .set_vnt_peer_summary_text(snapshot.peer_summary.into());
        if !snapshot.network_code.is_empty() && snapshot.network_code != "-" {
            self.ui.set_vnt_network_code(snapshot.network_code.into());
        }
        self.set_vnt_peer_rows(snapshot.peers.into_iter().map(vnt_peer_to_row).collect());
    }

    /// Reconciles the peer model in place for the ListView.
    fn set_vnt_peer_rows(&mut self, rows: Vec<VntPeerRow>) {
        while self.vnt_peer_model.row_count() > rows.len() {
            let _ = self
                .vnt_peer_model
                .remove(self.vnt_peer_model.row_count() - 1);
        }
        for (index, row) in rows.into_iter().enumerate() {
            if index < self.vnt_peer_model.row_count() {
                self.vnt_peer_model.set_row_data(index, row);
            } else {
                self.vnt_peer_model.push(row);
            }
        }
    }

    /// Starts installation/update on a worker thread.
    fn start_install(&mut self) {
        let target = match self.require_target() {
            Ok(target) => target,
            Err(error) => {
                self.show_error_dialog("安装", &error.to_string());
                return;
            }
        };
        let keep_topmost = self.ui.get_keep_topmost();
        let hotkey = self.ui.get_hotkey_text().to_string();
        if let Err(error) = core::normalize_hotkey(&hotkey) {
            self.show_error_dialog("安装", &error.to_string());
            return;
        }
        self.ui.set_busy(true);
        self.ui.set_status_text("安装中...".into());
        let core = self.core.clone();
        let tx = self.tx.clone();
        thread::spawn(move || match core.install(&target, keep_topmost, &hotkey) {
            Ok(dialog) => {
                let _ = tx.send(AppMessage::ActionFinished {
                    title: "安装".to_string(),
                    status: "完成".to_string(),
                    dialog,
                    process_status: None,
                    target: Some(target),
                    load_topmost: true,
                });
            }
            Err(error) => {
                let _ = tx.send(AppMessage::ActionFailed {
                    title: "安装".to_string(),
                    status: "执行失败".to_string(),
                    error: error.to_string(),
                });
            }
        });
    }

    /// Starts uninstall on a worker thread.
    fn start_uninstall(&mut self) {
        let target = match self.require_target() {
            Ok(target) => target,
            Err(error) => {
                self.show_error_dialog("卸载", &error.to_string());
                return;
            }
        };
        self.ui.set_busy(true);
        self.ui.set_status_text("卸载中...".into());
        let core = self.core.clone();
        let tx = self.tx.clone();
        thread::spawn(move || match core.uninstall(&target) {
            Ok(dialog) => {
                let _ = tx.send(AppMessage::ActionFinished {
                    title: "卸载".to_string(),
                    status: "完成".to_string(),
                    dialog,
                    process_status: Some("运行进程：未检测".to_string()),
                    target: Some(target),
                    load_topmost: false,
                });
            }
            Err(error) => {
                let _ = tx.send(AppMessage::ActionFailed {
                    title: "卸载".to_string(),
                    status: "执行失败".to_string(),
                    error: error.to_string(),
                });
            }
        });
    }

    /// Performs pre-launch validation and asks before closing port conflicts.
    fn start_launch(&mut self) {
        let target = match self.require_target() {
            Ok(target) => target,
            Err(error) => {
                self.show_error_dialog("启动游戏", &error.to_string());
                return;
            }
        };
        let keep_topmost = self.ui.get_keep_topmost();
        let hotkey = self.ui.get_hotkey_text().to_string();
        if let Err(error) = core::normalize_hotkey(&hotkey) {
            self.show_error_dialog("启动游戏", &error.to_string());
            return;
        }
        let conflicts = match self.core.collect_port_conflicts() {
            Ok(conflicts) => conflicts,
            Err(error) => {
                self.show_error_dialog("启动游戏", &error.to_string());
                return;
            }
        };
        if !conflicts.is_empty() {
            self.show_confirm_dialog(
                "端口占用检测",
                &format!(
                    "检测到启动所需端口已被占用：\n{}\n\n是否关闭这些占用端口的进程后继续启动？",
                    format_port_conflicts(&conflicts)
                ),
                "关闭并启动",
                "取消",
                PendingDialogAction::LaunchWithConflicts {
                    target,
                    keep_topmost,
                    hotkey,
                    conflicts,
                },
            );
            return;
        }
        self.start_launch_with_conflicts(target, keep_topmost, hotkey, conflicts);
    }

    /// Launches after optionally closing known port-conflict processes.
    fn start_launch_with_conflicts(
        &mut self,
        target: PathBuf,
        keep_topmost: bool,
        hotkey: String,
        conflicts: Vec<PortConflict>,
    ) {
        self.ui.set_busy(true);
        self.ui.set_status_text("启动游戏中...".into());
        let core = self.core.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = (|| -> Result<String> {
                if !conflicts.is_empty() {
                    core.stop_port_conflict_processes(&conflicts)?;
                    thread::sleep(Duration::from_secs(1));
                    let remaining = core.collect_port_conflicts()?;
                    if !remaining.is_empty() {
                        bail!(
                            "端口仍被占用，无法继续启动：\n{}",
                            format_port_conflicts(&remaining)
                        );
                    }
                }
                core.launch(&target, keep_topmost, &hotkey)
            })();

            match result {
                Ok(dialog) => {
                    let _ = tx.send(AppMessage::ActionFinished {
                        title: "启动游戏".to_string(),
                        status: "完成".to_string(),
                        dialog,
                        process_status: None,
                        target: Some(target),
                        load_topmost: true,
                    });
                }
                Err(error) => {
                    let _ = tx.send(AppMessage::ActionFailed {
                        title: "启动游戏".to_string(),
                        status: "执行失败".to_string(),
                        error: error.to_string(),
                    });
                }
            }
        });
    }

    /// Runs process and port diagnostics on a worker thread.
    fn start_detect_processes(&mut self) {
        let target = match self.require_target() {
            Ok(target) => target,
            Err(error) => {
                self.show_error_dialog("进程检测", &error.to_string());
                return;
            }
        };
        self.ui.set_busy(true);
        self.ui.set_status_text("进程检测中...".into());
        let core = self.core.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = (|| -> Result<(String, String)> {
                let snapshot = core.collect_runtime_processes(&target)?;
                let conflicts = core.collect_port_conflicts()?;
                let has_runtime = runtime_snapshot_has_any(&snapshot);
                let has_conflicts = !conflicts.is_empty();
                let message = format_process_detection_message(&snapshot, &conflicts);
                let status = match (has_runtime, has_conflicts) {
                    (true, true) => format!("检测到运行进程和 {} 个端口占用", conflicts.len()),
                    (true, false) => "检测到相关运行进程".to_string(),
                    (false, true) => format!("检测到 {} 个端口占用", conflicts.len()),
                    (false, false) => "未检测到相关进程或端口占用".to_string(),
                };
                Ok((message, status))
            })();

            match result {
                Ok((message, process_status)) => {
                    let _ = tx.send(AppMessage::ActionFinished {
                        title: "进程检测".to_string(),
                        status: "完成".to_string(),
                        dialog: message,
                        process_status: Some(process_status),
                        target: Some(target),
                        load_topmost: false,
                    });
                }
                Err(error) => {
                    let _ = tx.send(AppMessage::ActionFailed {
                        title: "进程检测".to_string(),
                        status: "执行失败".to_string(),
                        error: error.to_string(),
                    });
                }
            }
        });
    }

    /// Closes runtime and port-conflict processes from a worker thread.
    fn start_stop_processes(&mut self) {
        let target = match self.require_target() {
            Ok(target) => target,
            Err(error) => {
                self.show_error_dialog("关闭所有进程", &error.to_string());
                return;
            }
        };
        self.ui.set_busy(true);
        self.ui.set_status_text("关闭所有进程中...".into());
        let core = self.core.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = (|| -> Result<(String, String)> {
                let snapshot = core.collect_runtime_processes(&target)?;
                let conflicts = core.collect_port_conflicts()?;
                let has_runtime = runtime_snapshot_has_any(&snapshot);
                if !has_runtime && conflicts.is_empty() {
                    return Ok((
                        "当前没有需要关闭的运行进程或端口占用进程。".to_string(),
                        "运行进程/端口占用：未发现".to_string(),
                    ));
                }

                let mut messages = Vec::new();
                if has_runtime {
                    messages.push(core.stop_runtime_processes(&target)?);
                }
                if !conflicts.is_empty() {
                    messages.push(core.stop_port_conflict_processes(&conflicts)?);
                }

                thread::sleep(Duration::from_millis(800));
                let remaining = core.collect_port_conflicts()?;
                let process_status = if remaining.is_empty() {
                    "运行进程/端口占用：已请求关闭".to_string()
                } else {
                    format!("仍有 {} 个端口占用", remaining.len())
                };
                if !remaining.is_empty() {
                    messages.push(format!(
                        "仍检测到端口占用：\n{}",
                        format_port_conflicts(&remaining)
                    ));
                }

                Ok((messages.join("\n\n"), process_status))
            })();

            match result {
                Ok((dialog, process_status)) => {
                    let _ = tx.send(AppMessage::ActionFinished {
                        title: "关闭所有进程".to_string(),
                        status: "完成".to_string(),
                        dialog,
                        process_status: Some(process_status),
                        target: Some(target),
                        load_topmost: false,
                    });
                }
                Err(error) => {
                    let _ = tx.send(AppMessage::ActionFailed {
                        title: "关闭所有进程".to_string(),
                        status: "执行失败".to_string(),
                        error: error.to_string(),
                    });
                }
            }
        });
    }

    /// Shows a non-error in-app modal.
    fn show_info_dialog(&mut self, title: &str, text: &str) {
        self.show_app_dialog(title, text, "确定", "", false, false);
        self.pending_dialog_action = PendingDialogAction::None;
    }

    /// Shows an error in-app modal.
    fn show_error_dialog(&mut self, title: &str, text: &str) {
        self.show_app_dialog(title, text, "确定", "", false, true);
        self.pending_dialog_action = PendingDialogAction::None;
    }

    /// Shows an in-app confirmation modal and remembers the accepted action.
    fn show_confirm_dialog(
        &mut self,
        title: &str,
        text: &str,
        primary: &str,
        secondary: &str,
        action: PendingDialogAction,
    ) {
        self.show_app_dialog(title, text, primary, secondary, true, false);
        self.pending_dialog_action = action;
    }

    /// Shows the in-app manual path input modal.
    fn show_path_dialog(&mut self, initial: &str) {
        self.show_app_dialog(
            "选择游戏根目录",
            "输入或粘贴 Boundary 游戏根目录。可以填写游戏根目录，也可以填写 Binaries\\Win64 目录。",
            "应用路径",
            "取消",
            false,
            false,
        );
        self.ui.set_app_dialog_input(true);
        self.ui.set_app_dialog_input_text(initial.into());
        self.pending_dialog_action = PendingDialogAction::ManualPathInput;
    }

    /// Shared modal state setter used by info, error, confirm, and input flows.
    fn show_app_dialog(
        &mut self,
        title: &str,
        text: &str,
        primary: &str,
        secondary: &str,
        confirm: bool,
        error: bool,
    ) {
        self.ui.set_app_dialog_title(title.into());
        self.ui.set_app_dialog_text(text.into());
        self.ui.set_app_dialog_primary_text(primary.into());
        self.ui.set_app_dialog_secondary_text(secondary.into());
        self.ui.set_app_dialog_confirm(confirm);
        self.ui.set_app_dialog_input(false);
        self.ui.set_app_dialog_error(error);
        self.ui.set_show_app_dialog(true);
    }

    /// Clears modal state and any pending action.
    fn hide_app_dialog(&mut self) {
        self.ui.set_show_app_dialog(false);
        self.ui.set_app_dialog_confirm(false);
        self.ui.set_app_dialog_input(false);
        self.ui.set_app_dialog_error(false);
        self.pending_dialog_action = PendingDialogAction::None;
    }

    /// Handles the modal primary button.
    fn handle_dialog_primary(&mut self) {
        let action = std::mem::replace(&mut self.pending_dialog_action, PendingDialogAction::None);
        match action {
            PendingDialogAction::None => self.hide_app_dialog(),
            PendingDialogAction::ManualPathInput => self.confirm_manual_path_from_dialog(),
            PendingDialogAction::LaunchWithConflicts {
                target,
                keep_topmost,
                hotkey,
                conflicts,
            } => {
                self.hide_app_dialog();
                self.start_launch_with_conflicts(target, keep_topmost, hotkey, conflicts);
            }
        }
    }

    /// Handles modal cancellation and applies any cancel-side status changes.
    fn handle_dialog_secondary(&mut self) {
        if matches!(
            &self.pending_dialog_action,
            PendingDialogAction::LaunchWithConflicts { .. }
        ) {
            self.ui.set_status_text("已取消启动".into());
        }
        self.hide_app_dialog();
    }

    /// Validates and applies the manual path typed into the modal.
    fn confirm_manual_path_from_dialog(&mut self) {
        let raw = self.ui.get_app_dialog_input_text().to_string();
        if raw.trim().is_empty() {
            self.ui.set_app_dialog_title("路径无效".into());
            self.ui.set_app_dialog_text("游戏根目录不能为空。".into());
            self.ui.set_app_dialog_error(true);
            self.pending_dialog_action = PendingDialogAction::ManualPathInput;
            return;
        }

        match self.core.normalize_selected_path(Path::new(raw.trim())) {
            Ok(path) => {
                self.hide_app_dialog();
                self.mode = PathMode::Manual;
                self.ui.set_auto_mode(false);
                self.ui.set_manual_path(path.display().to_string().into());
                self.ui.set_detected_text("已手动设置游戏根目录".into());
                self.set_current_target(Some(path.clone()), "已就绪", true);
                self.append_log(&format!(
                    "[{}] 手动路径已设置：{}",
                    core::now_text(),
                    path.display()
                ));
            }
            Err(error) => {
                self.ui.set_app_dialog_title("路径无效".into());
                self.ui.set_app_dialog_text(error.to_string().into());
                self.ui.set_app_dialog_error(true);
                self.pending_dialog_action = PendingDialogAction::ManualPathInput;
            }
        }
    }

    /// Opens the log directory in Explorer.
    fn open_logs_dir(&self) {
        let _ = std::process::Command::new("explorer")
            .arg(self.core.installer_home.join("logs"))
            .spawn();
    }

    /// Stops background sessions before the UI exits.
    fn shutdown(&mut self) {
        if let Some(session) = self.vnt_session.as_mut() {
            session.stop();
        }
    }
}

/// Fetches and parses the remote community server list.
fn fetch_servers() -> Result<Vec<RemoteServer>> {
    let body = http_get_json(SERVER_LIST_HOST, SERVER_LIST_PORT, SERVER_LIST_PATH)
        .context("请求服务器列表接口失败")?;
    let servers =
        serde_json::from_str::<Vec<RemoteServer>>(&body).context("解析服务器列表 JSON 失败")?;
    Ok(servers)
}

/// Checks whether any runtime process group contains entries.
fn runtime_snapshot_has_any(snapshot: &RuntimeSnapshot) -> bool {
    !snapshot.game.is_empty()
        || !snapshot.wrapper.is_empty()
        || !snapshot.server.is_empty()
        || !snapshot.watcher.is_empty()
}

/// Slint reports Escape as a control character on this backend.
fn hotkey_capture_is_escape(text: &str) -> bool {
    text.starts_with('\u{001b}')
}

/// Builds a hotkey string from Slint's text/modifier fields.
fn hotkey_from_capture(
    text: &str,
    control: bool,
    alt: bool,
    shift: bool,
    meta: bool,
) -> Option<String> {
    let key = captured_key_label(text)?;
    let mut parts = Vec::new();
    if control {
        parts.push("Ctrl".to_string());
    }
    if alt {
        parts.push("Alt".to_string());
    }
    if shift {
        parts.push("Shift".to_string());
    }
    if meta {
        parts.push("Win".to_string());
    }
    parts.push(key);
    Some(parts.join("+"))
}

/// Maps Slint key text into the labels accepted by core::normalize_hotkey.
fn captured_key_label(text: &str) -> Option<String> {
    let mut chars = text.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    match ch {
        '\u{0010}' | '\u{0011}' | '\u{0012}' | '\u{0013}' | '\u{0017}' | '\u{0018}' => None,
        '\u{0009}' => Some("Tab".to_string()),
        '\u{000a}' => Some("Enter".to_string()),
        '\u{0020}' => Some("Space".to_string()),
        '\u{007f}' => Some("Delete".to_string()),
        '\u{F704}'..='\u{F71B}' => {
            let number = ch as u32 - '\u{F704}' as u32 + 1;
            Some(format!("F{number}"))
        }
        '\u{F727}' => Some("Insert".to_string()),
        '\u{F729}' => Some("Home".to_string()),
        '\u{F72B}' => Some("End".to_string()),
        '\u{F72C}' => Some("PageUp".to_string()),
        '\u{F72D}' => Some("PageDown".to_string()),
        value if value.is_ascii_alphabetic() => Some(value.to_ascii_uppercase().to_string()),
        value if value.is_ascii_digit() => Some(value.to_string()),
        _ => None,
    }
}

/// Builds the detailed process/port report shown in the diagnostics modal.
fn format_process_detection_message(
    snapshot: &RuntimeSnapshot,
    conflicts: &[PortConflict],
) -> String {
    let mut parts = vec![format!(
        "相关运行进程：{}",
        core::summarize_runtime_processes(snapshot)
    )];
    if conflicts.is_empty() {
        parts.push("端口占用：未发现。".to_string());
    } else {
        parts.push(format!("端口占用：\n{}", format_port_conflicts(conflicts)));
    }
    parts.join("\n\n")
}

/// Minimal HTTP/1.1 GET helper for JSON endpoints.
fn http_get_json(host: &str, port: u16, path: &str) -> Result<String> {
    let mut stream =
        TcpStream::connect((host, port)).with_context(|| format!("连接 {host}:{port} 失败"))?;
    stream.set_read_timeout(Some(Duration::from_secs(12)))?;
    stream.set_write_timeout(Some(Duration::from_secs(8)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUser-Agent: boundary-toolbox/1.2\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    )?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let header_end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("服务器响应缺少 HTTP 头")?;
    let header = String::from_utf8_lossy(&raw[..header_end]);
    let mut lines = header.lines();
    let status_line = lines.next().unwrap_or_default();
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);
    if !(200..300).contains(&status_code) {
        bail!("服务器返回 HTTP {status_code}");
    }

    // The server currently may return either a fixed Content-Length response or
    // transfer-encoding: chunked. Decode both so the UI list is robust.
    let is_chunked = lines.any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("transfer-encoding:") && lower.contains("chunked")
    });
    let body_bytes = &raw[header_end + 4..];
    let decoded = if is_chunked {
        decode_chunked_body(body_bytes)?
    } else {
        body_bytes.to_vec()
    };
    String::from_utf8(decoded).context("服务器列表响应不是 UTF-8")
}

/// Decodes a small HTTP chunked body.
fn decode_chunked_body(mut body: &[u8]) -> Result<Vec<u8>> {
    let mut decoded = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|window| window == b"\r\n")
            .context("chunked 响应格式错误")?;
        let size_text = String::from_utf8_lossy(&body[..line_end]);
        let size_hex = size_text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_hex, 16)
            .with_context(|| format!("无效 chunk 大小：{size_hex}"))?;
        body = &body[line_end + 2..];
        if size == 0 {
            break;
        }
        if body.len() < size + 2 {
            bail!("chunked 响应正文不完整");
        }
        decoded.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
    Ok(decoded)
}

/// Converts remote server JSON into a compact UI row.
fn server_to_row(server: RemoteServer) -> ServerRow {
    let state = normalize_server_state(&server.server_state);
    let active = state != "状态未知";
    ServerRow {
        name: shorten_text(&server.name, 44).into(),
        address: format!("{}:{}", empty_as_dash(&server.ip), server.port).into(),
        meta: format!(
            "{} / {} / {} / 更新时间 {}",
            empty_as_dash(&server.region),
            empty_as_dash(&server.mode),
            empty_as_dash(&server.map),
            format_heartbeat(server.last_heartbeat)
        )
        .into(),
        state: state.into(),
        players: format!("{} 人", server.player_count).into(),
        active,
    }
}

/// Placeholder row used while loading or after an error.
fn server_placeholder_row(title: &str, detail: &str) -> ServerRow {
    ServerRow {
        name: title.into(),
        address: detail.into(),
        meta: "服务器列表".into(),
        state: "WAIT".into(),
        players: "--".into(),
        active: false,
    }
}

/// Applies the default disconnected VNT state to Slint properties.
fn apply_vnt_idle_to_ui(ui: &AppWindow) {
    let snapshot = vnt_platform::idle_snapshot();
    ui.set_vnt_busy(snapshot.busy);
    ui.set_vnt_running(snapshot.running);
    ui.set_vnt_status_text(snapshot.status.into());
    ui.set_vnt_detail_text(snapshot.detail.into());
    ui.set_vnt_ip_text(snapshot.virtual_ip.into());
    ui.set_vnt_server_status_text(snapshot.server.into());
    ui.set_vnt_nat_text(snapshot.nat.into());
    ui.set_vnt_peer_summary_text(snapshot.peer_summary.into());
}

/// Default peer rows shown before VNT is running.
fn vnt_placeholder_rows() -> Vec<VntPeerRow> {
    vnt_platform::idle_snapshot()
        .peers
        .into_iter()
        .map(vnt_peer_to_row)
        .collect()
}

/// Maps a VNT peer snapshot into a Slint row.
fn vnt_peer_to_row(peer: VntPeer) -> VntPeerRow {
    VntPeerRow {
        name: peer.name.into(),
        address: peer.address.into(),
        detail: peer.detail.into(),
        online: peer.online,
    }
}

/// Normalizes empty/invalid server state strings.
fn normalize_server_state(state: &str) -> String {
    match state.trim() {
        "" | "InvalidState" => "状态未知".to_string(),
        value => value.to_string(),
    }
}

/// Displays blank remote fields as '-'.
fn empty_as_dash(value: &str) -> &str {
    if value.trim().is_empty() {
        "-"
    } else {
        value.trim()
    }
}

/// Keeps long server names from overflowing list rows.
fn shorten_text(value: &str, max_chars: usize) -> String {
    let mut text = value.trim().to_string();
    if text.chars().count() <= max_chars {
        return text;
    }
    text = text.chars().take(max_chars.saturating_sub(3)).collect();
    text.push_str("...");
    text
}

/// Formats server last-heartbeat timestamp in local time.
fn format_heartbeat(timestamp_ms: i64) -> String {
    if timestamp_ms <= 0 {
        return "-".to_string();
    }
    chrono::DateTime::from_timestamp_millis(timestamp_ms)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "-".to_string())
}

/// Periodically refreshes port status until the app exits.
fn spawn_port_thread(core: Arc<InstallerCore>, tx: Sender<AppMessage>, stop: Arc<AtomicBool>) {
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            if let Ok(rows) = core.port_status_rows() {
                let _ = tx.send(AppMessage::PortRows(rows));
            }
            thread::sleep(Duration::from_secs(2));
        }
    });
}
