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
        let active_page = Arc::new(AtomicI32::new(0));
        let port_target = Arc::new(RwLock::new(None));
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
        let mut app_prefs = match AppPrefs::load(&core.installer_home) {
            Ok(prefs) => prefs,
            Err(error) => {
                let backup_text = match AppPrefs::preserve_invalid(&core.installer_home) {
                    Ok(Some(path)) => format!("；已备份到 {}", path.display()),
                    Ok(None) => String::new(),
                    Err(backup_error) => format!("；备份损坏配置失败：{backup_error}"),
                };
                writeln!(
                    session_log_file,
                    "[{}] 应用配置读取失败，使用默认值：{}{}",
                    core::now_text(),
                    error,
                    backup_text
                )?;
                AppPrefs::default()
            }
        };
        app_prefs.language = i18n::normalize_language(app_prefs.language);
        let language = app_prefs.language;
        let vnt_prefs = app_prefs.vnt.clone();
        ui.set_language(language);

        // 对 Slint 模型只创建一次，后续原地更新，避免破坏已有 ListView 绑定。
        let port_model = Rc::new(VecModel::from(
            MONITORED_PORTS
                .iter()
                .map(|(protocol, port)| PortRow {
                    label: SharedString::from(format!("{}/{}", protocol, port)),
                    detail: SharedString::from(i18n::tr(
                        language,
                        "检测中...",
                        "Checking...",
                        "確認中...",
                    )),
                    protocol: SharedString::from(*protocol),
                    port: i32::from(*port),
                    pid: 0,
                    occupied: false,
                    expected: false,
                })
                .collect::<Vec<_>>(),
        ));
        let drive_model = Rc::new(VecModel::<DriveRow>::default());
        let server_model = Rc::new(VecModel::from(vec![server_placeholder_row(
            i18n::tr(
                language,
                "正在加载服务器列表",
                "Loading server list",
                "サーバー一覧を読み込み中",
            ),
            i18n::tr(
                language,
                "等待接口返回数据",
                "Waiting for the API response",
                "API 応答待ち",
            ),
            language,
        )]));
        let vnt_server_option_model = Rc::new(VecModel::from(
            vnt_prefs
                .server_options
                .iter()
                .map(|server| SharedString::from(server.as_str()))
                .collect::<Vec<_>>(),
        ));
        let vnt_server_model = Rc::new(VecModel::from(vnt_server_placeholder_rows(language)));
        let vnt_peer_model = Rc::new(VecModel::from(vnt_placeholder_rows(language)));
        ui.set_port_rows(ModelRc::from(port_model.clone()));
        ui.set_drive_rows(ModelRc::from(drive_model.clone()));
        ui.set_server_rows(ModelRc::from(server_model.clone()));
        ui.set_vnt_server_options(ModelRc::from(vnt_server_option_model.clone()));
        ui.set_vnt_server_rows(ModelRc::from(vnt_server_model.clone()));
        ui.set_vnt_peer_rows(ModelRc::from(vnt_peer_model.clone()));
        ui.set_payload_label(core.payload_label().into());
        ui.set_detected_text(
            i18n::tr(
                language,
                "正在检测 Steam 安装目录...",
                "Detecting Steam install path...",
                "Steam のインストール先を検出中...",
            )
            .into(),
        );
        ui.set_target_text(
            i18n::tr(
                language,
                "未解析到有效的安装目录",
                "No valid install path resolved",
                "有効なインストール先が見つかりません",
            )
            .into(),
        );
        ui.set_status_text(
            format!(
                "{} / v{}",
                i18n::tr(language, "准备就绪", "Ready", "準備完了"),
                APP_VERSION
            )
            .into(),
        );
        ui.set_process_status_text(
            i18n::tr(
                language,
                "运行进程：未检测",
                "Runtime processes: not checked",
                "実行中プロセス: 未確認",
            )
            .into(),
        );
        ui.set_show_logs(false);
        ui.set_busy(false);
        ui.set_pulse(false);
        ui.set_install_progress_visible(false);
        ui.set_install_progress_value(0.0);
        ui.set_install_progress_percent("0%".into());
        ui.set_install_progress_title("".into());
        ui.set_install_progress_detail("".into());
        ui.set_auto_mode(true);
        ui.set_has_target(false);
        ui.set_servers_loading(false);
        ui.set_server_status_text(
            i18n::tr(
                language,
                "服务器列表：未刷新",
                "Server list: not refreshed",
                "サーバー一覧: 未更新",
            )
            .into(),
        );
        ui.set_update_checking(false);
        ui.set_update_status_text(
            i18n::tr(
                language,
                "更新：未检查",
                "Update: not checked",
                "更新: 未確認",
            )
            .into(),
        );
        ui.set_show_drive_dialog(false);
        ui.set_show_app_dialog(false);
        ui.set_app_dialog_confirm(false);
        ui.set_app_dialog_input(false);
        ui.set_app_dialog_error(false);
        ui.set_app_dialog_title("".into());
        ui.set_app_dialog_text("".into());
        ui.set_app_dialog_input_text("".into());
        ui.set_app_dialog_primary_text(i18n::tr(language, "确定", "OK", "OK").into());
        ui.set_app_dialog_secondary_text(i18n::tr(language, "取消", "Cancel", "キャンセル").into());
        ui.set_vnt_server_text(vnt_prefs.server_text.into());
        ui.set_vnt_new_server_text("".into());
        ui.set_vnt_network_code(vnt_prefs.network_code.into());
        ui.set_vnt_password(vnt_prefs.password.into());
        ui.set_vnt_no_tun(vnt_prefs.no_tun);
        ui.set_vnt_compress(vnt_prefs.compress);
        ui.set_vnt_rtx(vnt_prefs.rtx);
        ui.set_vnt_busy(false);
        ui.set_vnt_running(false);
        apply_vnt_idle_to_ui(&ui, language);

        let controller = Rc::new(RefCell::new(Self {
            ui,
            core,
            tx,
            rx,
            stop_background: Arc::new(AtomicBool::new(false)),
            active_page,
            port_target,
            session_log_file,
            mode: PathMode::Auto,
            current_target: None,
            drive_model,
            port_model,
            server_model,
            vnt_server_option_model,
            vnt_server_model,
            vnt_peer_model,
            vnt_session: None,
            app_prefs,
            pending_dialog_action: PendingDialogAction::None,
        }));

        // 端口监控会立即启动；目标目录相关检查只在用户执行诊断操作时运行。
        spawn_port_thread(
            controller.borrow().core.clone(),
            controller.borrow().tx.clone(),
            controller.borrow().stop_background.clone(),
            controller.borrow().active_page.clone(),
            controller.borrow().port_target.clone(),
        );
        Ok(controller)
    }

    /// 将所有 Slint 回调绑定到控制器。
    pub(super) fn bind_callbacks(controller: &Rc<RefCell<Self>>) {
        let ui = controller.borrow().ui.as_weak();

        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_page_changed(move |page| {
                controller.borrow_mut().on_page_changed(page);
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_language_changed(move |language| {
                controller.borrow_mut().set_language(language);
            });
        }
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
            ui.unwrap().on_install_clicked(move || {
                controller.borrow_mut().start_install();
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_install_progress_closed(move || {
                let controller = controller.borrow();
                if !controller.ui.get_busy() {
                    controller.ui.set_install_progress_visible(false);
                }
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
            ui.unwrap().on_launch_pvp_clicked(move || {
                controller.borrow_mut().start_launch(LaunchMode::Pvp);
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_launch_pve_clicked(move || {
                controller.borrow_mut().start_launch(LaunchMode::Pve);
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
            ui.unwrap().on_check_updates_clicked(move || {
                controller.borrow_mut().start_update_check(false);
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
            ui.unwrap().on_vnt_add_server_clicked(move |address| {
                controller
                    .borrow_mut()
                    .add_vnt_server_option(address.to_string());
            });
        }
        {
            let controller = Rc::clone(controller);
            ui.unwrap().on_vnt_settings_changed(move || {
                controller.borrow_mut().save_app_prefs();
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
        {
            let ui = controller.borrow().ui.as_weak();
            ui.unwrap().on_window_drag_started(move || {
                if let Some(app) = ui.upgrade() {
                    use slint::winit_030::WinitWindowAccessor;
                    app.window().with_winit_window(|window| {
                        let _ = window.drag_window();
                    });
                }
            });
        }
        {
            let ui = controller.borrow().ui.as_weak();
            ui.unwrap().on_window_minimize_clicked(move || {
                if let Some(app) = ui.upgrade() {
                    app.window().set_minimized(true);
                }
            });
        }
        {
            let ui = controller.borrow().ui.as_weak();
            ui.unwrap().on_window_close_clicked(move || {
                if let Some(app) = ui.upgrade() {
                    let _ = app.hide();
                }
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
        let installing_font = self.start_ui_font_check();
        self.refresh_target_from_mode(true);
        self.start_refresh_servers();
        if !installing_font {
            self.start_update_check(true);
        }
    }

    pub(super) fn on_page_changed(&mut self, page: i32) {
        self.active_page.store(page, Ordering::Relaxed);
        if page == 2 {
            self.refresh_port_rows_once();
        }
    }

    fn refresh_port_rows_once(&self) {
        let core = self.core.clone();
        let tx = self.tx.clone();
        let target = self.port_target.read().ok().and_then(|guard| guard.clone());
        thread::spawn(move || {
            if let Ok(rows) = core.port_status_rows_for_target(target.as_deref()) {
                let _ = tx.send(AppMessage::PortRows(rows));
            }
        });
    }

    /// 在 UI 退出前停止后台会话。
    pub(super) fn shutdown(&mut self) {
        self.save_app_prefs();
        if let Some(session) = self.vnt_session.as_mut() {
            session.stop();
        }
    }
}
