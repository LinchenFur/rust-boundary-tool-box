//! AppController 安装、卸载、启动和诊断动作。

use super::*;

impl AppController {
    /// 在工作线程中启动安装或更新。
    pub(super) fn start_install(&mut self) {
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

    /// 在工作线程中启动卸载。
    pub(super) fn start_uninstall(&mut self) {
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

    /// 执行启动前校验，并在关闭端口冲突进程前请求确认。
    pub(super) fn start_launch(&mut self) {
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

    /// 可选关闭已知端口冲突进程后启动游戏。
    pub(super) fn start_launch_with_conflicts(
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

    /// 在工作线程中运行进程和端口诊断。
    pub(super) fn start_detect_processes(&mut self) {
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

    /// 在工作线程中关闭运行时进程和端口冲突进程。
    pub(super) fn start_stop_processes(&mut self) {
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
}
