//! AppController 安装、卸载、启动和诊断动作。

use super::*;

impl AppController {
    /// 在工作线程中启动安装或更新。
    pub(super) fn start_install(&mut self) {
        let target = match self.require_target() {
            Ok(target) => target,
            Err(error) => {
                self.show_error_dialog(
                    self.tr("安装", "Install", "インストール"),
                    &error.to_string(),
                );
                return;
            }
        };
        self.ui.set_busy(true);
        self.ui.set_status_text(
            self.tr("安装中...", "Installing...", "インストール中...")
                .into(),
        );
        self.ui.set_install_progress_visible(true);
        self.ui.set_install_progress_value(0.0);
        self.ui.set_install_progress_percent("0%".into());
        self.ui.set_install_progress_title(
            self.tr("准备安装", "Preparing install", "インストール準備中")
                .into(),
        );
        self.ui
            .set_install_progress_detail(target.display().to_string().into());
        let core = self.core.clone();
        let tx = self.tx.clone();
        let progress_tx = tx.clone();
        let progress = Arc::new(move |progress: InstallProgress| {
            let _ = progress_tx.send(AppMessage::InstallProgress(progress));
        });
        thread::spawn(
            move || match core.install_with_progress(&target, progress) {
                Ok(dialog) => {
                    let _ = tx.send(AppMessage::ActionFinished {
                        title: "安装".to_string(),
                        status: "完成".to_string(),
                        dialog,
                        process_status: None,
                        target: Some(target),
                    });
                }
                Err(error) => {
                    let _ = tx.send(AppMessage::ActionFailed {
                        title: "安装".to_string(),
                        status: "执行失败".to_string(),
                        error: error.to_string(),
                    });
                }
            },
        );
    }

    /// 在工作线程中启动卸载。
    pub(super) fn start_uninstall(&mut self) {
        let target = match self.require_target() {
            Ok(target) => target,
            Err(error) => {
                self.show_error_dialog(
                    self.tr("卸载", "Uninstall", "アンインストール"),
                    &error.to_string(),
                );
                return;
            }
        };
        let language = self.language();
        self.ui.set_busy(true);
        self.ui.set_status_text(
            self.tr("卸载中...", "Uninstalling...", "アンインストール中...")
                .into(),
        );
        self.ui.set_install_progress_visible(false);
        let core = self.core.clone();
        let tx = self.tx.clone();
        thread::spawn(move || match core.uninstall(&target) {
            Ok(dialog) => {
                let _ = tx.send(AppMessage::ActionFinished {
                    title: "卸载".to_string(),
                    status: "完成".to_string(),
                    dialog,
                    process_status: Some(
                        i18n::tr(
                            language,
                            "运行进程：未检测",
                            "Runtime processes: not checked",
                            "実行中プロセス: 未確認",
                        )
                        .to_string(),
                    ),
                    target: Some(target),
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

    /// 执行启动前校验，并在需要本地登录服务器时请求确认端口冲突处理。
    pub(super) fn start_launch(&mut self, mode: LaunchMode) {
        let title = format!("启动 {}", mode.display_name());
        let display_title = self.localize_action_title(&title);
        let target = match self.require_target() {
            Ok(target) => target,
            Err(error) => {
                self.show_error_dialog(&display_title, &error.to_string());
                return;
            }
        };

        let conflicts = if mode.uses_login_server() {
            match self.core.collect_port_conflicts() {
                Ok(conflicts) => conflicts,
                Err(error) => {
                    self.show_error_dialog(&display_title, &error.to_string());
                    return;
                }
            }
        } else {
            Vec::new()
        };
        if mode.uses_login_server() && !conflicts.is_empty() {
            self.show_confirm_dialog(
                &format!(
                    "{} {}",
                    mode.display_name(),
                    self.tr("端口占用检测", "Port Conflict Check", "ポート使用確認")
                ),
                &format!(
                    "{} {} {}\n{}\n\n{}",
                    self.tr("检测到", "Ports required by", ""),
                    mode.display_name(),
                    self.tr(
                        "启动所需端口已被占用：",
                        "launch are already in use:",
                        " の起動に必要なポートが使用中です:"
                    ),
                    format_port_conflicts(&conflicts),
                    self.tr(
                        "是否关闭这些占用端口的进程后继续启动？",
                        "Stop those processes and continue launching?",
                        "これらのプロセスを停止して起動を続行しますか？"
                    )
                ),
                &format!(
                    "{} {}",
                    self.tr("关闭并启动", "Stop and launch", "停止して起動"),
                    mode.display_name()
                ),
                self.tr("取消", "Cancel", "キャンセル"),
                PendingDialogAction::LaunchWithConflicts {
                    target,
                    mode,
                    conflicts,
                },
            );
            return;
        }
        self.start_launch_with_conflicts(target, mode, conflicts);
    }

    /// 可选关闭已知端口冲突进程后启动指定模式。
    pub(super) fn start_launch_with_conflicts(
        &mut self,
        target: PathBuf,
        mode: LaunchMode,
        conflicts: Vec<PortConflict>,
    ) {
        self.ui.set_busy(true);
        self.ui.set_install_progress_visible(false);
        self.ui.set_status_text(
            format!(
                "{} {}...",
                self.tr("启动中", "Launching", "起動中"),
                mode.display_name()
            )
            .into(),
        );
        let language = self.language();
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
                            "{}\n{}",
                            i18n::tr(
                                language,
                                "端口仍被占用，无法继续启动：",
                                "Ports are still in use; launch cannot continue:",
                                "ポートがまだ使用中のため、起動を続行できません:"
                            ),
                            format_port_conflicts(&remaining)
                        );
                    }
                }
                core.launch(&target, mode)
            })();

            match result {
                Ok(dialog) => {
                    let _ = tx.send(AppMessage::ActionFinished {
                        title: format!("启动 {}", mode.display_name()),
                        status: "完成".to_string(),
                        dialog,
                        process_status: None,
                        target: Some(target),
                    });
                }
                Err(error) => {
                    let _ = tx.send(AppMessage::ActionFailed {
                        title: format!("启动 {}", mode.display_name()),
                        status: "执行失败".to_string(),
                        error: error.to_string(),
                    });
                }
            }
        });
    }

    /// 在工作线程中运行进程和端口诊断。
    pub(super) fn start_detect_processes(&mut self) {
        let target = match self.require_target() {
            Ok(target) => target,
            Err(error) => {
                self.show_error_dialog(
                    self.tr("进程检测", "Process Detection", "プロセス検出"),
                    &error.to_string(),
                );
                return;
            }
        };
        let language = self.language();
        self.ui.set_busy(true);
        self.ui.set_status_text(
            self.tr(
                "进程检测中...",
                "Detecting processes...",
                "プロセス検出中...",
            )
            .into(),
        );
        self.ui.set_install_progress_visible(false);
        let core = self.core.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = (|| -> Result<(String, String)> {
                let snapshot = core.collect_runtime_processes(&target)?;
                let conflicts = core.collect_port_conflicts()?;
                let has_runtime = runtime_snapshot_has_any(&snapshot);
                let has_conflicts = !conflicts.is_empty();
                let message = format_process_detection_message(&snapshot, &conflicts, language);
                let status = match (has_runtime, has_conflicts) {
                    (true, true) => format!(
                        "{} {} {}",
                        i18n::tr(
                            language,
                            "检测到运行进程和",
                            "Found runtime processes and",
                            "実行中プロセスと"
                        ),
                        conflicts.len(),
                        i18n::tr(language, "个端口占用", "port conflicts", "件のポート使用")
                    ),
                    (true, false) => i18n::tr(
                        language,
                        "检测到相关运行进程",
                        "Related runtime processes found",
                        "関連実行プロセスが見つかりました",
                    )
                    .to_string(),
                    (false, true) => format!(
                        "{} {} {}",
                        i18n::tr(language, "检测到", "Found", ""),
                        conflicts.len(),
                        i18n::tr(language, "个端口占用", "port conflicts", "件のポート使用")
                    ),
                    (false, false) => i18n::tr(
                        language,
                        "未检测到相关进程或端口占用",
                        "No related processes or port conflicts found",
                        "関連プロセスまたはポート使用は見つかりません",
                    )
                    .to_string(),
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

    /// 在工作线程中只关闭某一个端口行对应的占用进程。
    pub(super) fn start_stop_port_process(&mut self, protocol: String, port: i32, pid: i32) {
        let protocol = protocol.trim().to_uppercase();
        if protocol.is_empty() || port <= 0 || port > u16::MAX as i32 || pid <= 0 {
            self.show_error_dialog(
                self.tr("关闭端口进程", "Stop Port Process", "ポートプロセス停止"),
                self.tr(
                    "端口行没有可关闭的有效进程。",
                    "This port row has no valid process to stop.",
                    "このポート行には停止できる有効なプロセスがありません。",
                ),
            );
            return;
        }

        self.ui.set_busy(true);
        self.ui.set_install_progress_visible(false);
        self.ui.set_status_text(
            format!(
                "{} {}/{} PID {}...",
                self.tr("关闭中", "Stopping", "停止中"),
                protocol,
                port,
                pid
            )
            .into(),
        );
        let language = self.language();
        let core = self.core.clone();
        let tx = self.tx.clone();
        let target = self.current_target.clone();
        thread::spawn(move || {
            let result = (|| -> Result<(String, String, Vec<CorePortStatusRow>)> {
                let current = core.collect_port_conflicts()?;
                let conflict = current
                    .into_iter()
                    .find(|conflict| {
                        conflict.protocol.eq_ignore_ascii_case(&protocol)
                            && i32::from(conflict.port) == port
                            && i32::try_from(conflict.pid).ok() == Some(pid)
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "{}",
                            format!(
                                "{}/{} PID {} {}",
                                protocol,
                                port,
                                pid,
                                i18n::tr(
                                    language,
                                    "的端口占用已经不存在，请等待列表刷新。",
                                    "is no longer using this port. Wait for the list to refresh.",
                                    "のポート使用は既に存在しません。リストの更新をお待ちください。"
                                )
                            )
                        )
                    })?;

                let summary_before = format_port_conflicts(std::slice::from_ref(&conflict));
                core.stop_port_conflict_processes(std::slice::from_ref(&conflict))?;
                thread::sleep(Duration::from_millis(800));

                let remaining = core.collect_port_conflicts()?;
                let still_exists = remaining.iter().any(|item| {
                    item.protocol.eq_ignore_ascii_case(&protocol)
                        && i32::from(item.port) == port
                        && i32::try_from(item.pid).ok() == Some(pid)
                });
                let rows = core.port_status_rows_for_target(target.as_deref())?;
                let status = if still_exists {
                    format!(
                        "{}/{} PID {} {}",
                        protocol,
                        port,
                        pid,
                        i18n::tr(language, "仍在占用", "still in use", "まだ使用中")
                    )
                } else {
                    format!(
                        "{}/{} PID {} {}",
                        protocol,
                        port,
                        pid,
                        i18n::tr(
                            language,
                            "已请求关闭",
                            "stop requested",
                            "停止を要求しました"
                        )
                    )
                };
                let dialog = if still_exists {
                    format!(
                        "{}\n{}",
                        i18n::tr(
                            language,
                            "已请求关闭该端口进程，但仍检测到占用：",
                            "Stop was requested, but the port is still in use:",
                            "停止を要求しましたが、まだポート使用が検出されました:"
                        ),
                        summary_before
                    )
                } else {
                    format!(
                        "{}\n{}",
                        i18n::tr(
                            language,
                            "已关闭该端口对应进程：",
                            "Stopped the process for this port:",
                            "このポートのプロセスを停止しました:"
                        ),
                        summary_before
                    )
                };
                Ok((dialog, status, rows))
            })();

            match result {
                Ok((dialog, process_status, rows)) => {
                    let _ = tx.send(AppMessage::PortRows(rows));
                    let _ = tx.send(AppMessage::ActionFinished {
                        title: "关闭端口进程".to_string(),
                        status: "完成".to_string(),
                        dialog,
                        process_status: Some(process_status),
                        target: None,
                    });
                }
                Err(error) => {
                    let _ = tx.send(AppMessage::ActionFailed {
                        title: "关闭端口进程".to_string(),
                        status: "执行失败".to_string(),
                        error: error.to_string(),
                    });
                }
            }
        });
    }

    /// 在工作线程中关闭运行时进程和端口冲突进程。
    pub(super) fn start_stop_processes(&mut self) {
        let target = match self.require_target() {
            Ok(target) => target,
            Err(error) => {
                self.show_error_dialog(
                    self.tr(
                        "关闭所有进程",
                        "Stop All Processes",
                        "すべてのプロセスを停止",
                    ),
                    &error.to_string(),
                );
                return;
            }
        };
        self.ui.set_busy(true);
        self.ui.set_status_text(
            self.tr(
                "关闭所有进程中...",
                "Stopping all processes...",
                "すべてのプロセスを停止中...",
            )
            .into(),
        );
        self.ui.set_install_progress_visible(false);
        let language = self.language();
        let core = self.core.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = (|| -> Result<(String, String)> {
                let snapshot = core.collect_runtime_processes(&target)?;
                let mut conflicts = core.collect_port_conflicts()?;
                let has_runtime = runtime_snapshot_has_any(&snapshot);
                let had_initial_conflicts = !conflicts.is_empty();

                let mut messages = Vec::new();
                // 即使目标目录扫描没有命中，也要进入核心关闭逻辑；那里会按
                // Boundary 专用镜像名兜底处理 wrapper/game 残留进程。
                let runtime_message = core.stop_runtime_processes(&target)?;
                let runtime_was_empty = runtime_message.starts_with("当前没有需要关闭");
                messages.push(runtime_message);
                conflicts = core.collect_port_conflicts()?;
                if !conflicts.is_empty() {
                    messages.push(core.stop_port_conflict_processes(&conflicts)?);
                }

                thread::sleep(Duration::from_millis(800));
                let remaining_conflicts = core.collect_port_conflicts()?;
                let remaining_runtime = core.collect_runtime_processes(&target)?;
                let has_remaining_runtime = runtime_snapshot_has_any(&remaining_runtime);
                let process_status = if runtime_was_empty
                    && !has_runtime
                    && !had_initial_conflicts
                    && remaining_conflicts.is_empty()
                    && !has_remaining_runtime
                {
                    i18n::tr(
                        language,
                        "运行进程/端口占用：未发现",
                        "Runtime/port usage: none found",
                        "実行プロセス/ポート使用: 見つかりません",
                    )
                    .to_string()
                } else if remaining_conflicts.is_empty() && !has_remaining_runtime {
                    i18n::tr(
                        language,
                        "运行进程/端口占用：已关闭",
                        "Runtime/port usage: stopped",
                        "実行プロセス/ポート使用: 停止済み",
                    )
                    .to_string()
                } else if has_remaining_runtime {
                    i18n::tr(
                        language,
                        "仍有相关运行进程",
                        "Related runtime processes remain",
                        "関連実行プロセスが残っています",
                    )
                    .to_string()
                } else {
                    format!(
                        "{} {} {}",
                        i18n::tr(language, "仍有", "Still", ""),
                        remaining_conflicts.len(),
                        i18n::tr(
                            language,
                            "个端口占用",
                            "port conflicts remain",
                            "件のポート使用が残っています"
                        )
                    )
                };
                if has_remaining_runtime {
                    messages.push(format!(
                        "{}\n{}",
                        i18n::tr(
                            language,
                            "仍检测到相关运行进程：",
                            "Related runtime processes are still detected:",
                            "関連実行プロセスがまだ検出されています:"
                        ),
                        summarize_runtime_processes(&remaining_runtime, language)
                    ));
                }
                if !remaining_conflicts.is_empty() {
                    messages.push(format!(
                        "{}\n{}",
                        i18n::tr(
                            language,
                            "仍检测到端口占用：",
                            "Port usage is still detected:",
                            "ポート使用がまだ検出されています:"
                        ),
                        format_port_conflicts(&remaining_conflicts)
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
}
