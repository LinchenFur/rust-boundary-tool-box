//! AppController 生命周期和 Slint 回调绑定。

use super::*;

impl AppController {
    /// 创建模型、日志文件、核心服务和初始 UI 状态。
    pub(super) fn new(ui: AppWindow) -> Result<Rc<RefCell<Self>>> {
        let (tx, rx) = unbounded();
        // 来自 Core 的日志可能来自工作线程，因此和其它后台结果一样走 UI 安全的 channel。
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

        // 对 Slint 模型只创建一次，后续原地更新，避免破坏已有 ListView 绑定。
        let port_model = Rc::new(VecModel::from(
            MONITORED_PORTS
                .iter()
                .map(|(protocol, port)| PortRow {
                    label: SharedString::from(format!("{}/{}", protocol, port)),
                    detail: SharedString::from("检测中..."),
                    protocol: SharedString::from(*protocol),
                    port: i32::from(*port),
                    pid: 0,
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

        // 端口监控会立即启动；目标目录相关检查只在用户执行诊断操作时运行。
        spawn_port_thread(
            controller.borrow().core.clone(),
            controller.borrow().tx.clone(),
            controller.borrow().stop_background.clone(),
        );
        Ok(controller)
    }

    /// 将所有 Slint 回调绑定到控制器。
    pub(super) fn bind_callbacks(controller: &Rc<RefCell<Self>>) {
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
            ui.unwrap()
                .on_stop_port_process_clicked(move |protocol, port, pid| {
                    controller.borrow_mut().start_stop_port_process(
                        protocol.to_string(),
                        port,
                        pid,
                    );
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

    /// 启动 UI 线程定时器，用于拉取 channel 消息和驱动忙碌动画。
    pub(super) fn start_background_timers(controller: &Rc<RefCell<Self>>) {
        let log_controller = Rc::clone(controller);
        let log_timer = Timer::default();
        // 因为 Slint UI 对象不是 Send，所以工作线程只入队消息；
        // 该定时器在 UI 线程应用这些消息。
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

    /// 执行初始自动识别和服务器列表刷新。
    pub(super) fn initialize(&mut self) {
        self.refresh_target_from_mode(true);
        self.start_refresh_servers();
    }

    /// 在 UI 退出前停止后台会话。
    pub(super) fn shutdown(&mut self) {
        if let Some(session) = self.vnt_session.as_mut() {
            session.stop();
        }
    }
}
