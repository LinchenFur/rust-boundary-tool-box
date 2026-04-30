//! AppController 后台消息、日志和列表模型更新。

use super::*;

impl AppController {
    /// 同时向会话日志文件和可见日志面板追加一行。
    pub(super) fn append_log(&mut self, message: &str) {
        let _ = writeln!(self.session_log_file, "{}", message);
        let current = self.ui.get_log_text().to_string();
        let next = if current.is_empty() {
            message.to_string()
        } else {
            format!("{current}\n{message}")
        };
        self.ui.set_log_text(next.into());
    }

    /// 将队列中的工作线程消息应用到 UI 状态。
    pub(super) fn drain_messages(&mut self) {
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
                AppMessage::UpdateCheckFinished { result, automatic } => {
                    self.ui.set_update_checking(false);
                    self.ui
                        .set_update_status_text(update_status_text(&result).into());
                    self.append_log(&format!(
                        "[{}] 更新检查完成：当前 v{}，最新 {}",
                        core::now_text(),
                        APP_VERSION,
                        result.latest_tag
                    ));
                    let title = if automatic {
                        "自动检查更新"
                    } else {
                        "检查更新"
                    };
                    self.show_info_dialog(title, &update_dialog_text(&result));
                }
                AppMessage::UpdateCheckFailed { error, automatic } => {
                    self.ui.set_update_checking(false);
                    self.ui
                        .set_update_status_text(format!("更新：检查失败：{error}").into());
                    self.append_log(&format!("[{}] 更新检查失败：{}", core::now_text(), error));
                    let title = if automatic {
                        "自动检查更新"
                    } else {
                        "检查更新"
                    };
                    self.show_error_dialog(title, &error);
                }
                AppMessage::VntEvent(event) => self.apply_vnt_event(event),
                AppMessage::ActionFinished {
                    title,
                    status,
                    dialog,
                    process_status,
                    target,
                } => {
                    self.ui.set_busy(false);
                    self.ui.set_status_text(status.into());
                    if let Some(process_status) = process_status {
                        self.ui.set_process_status_text(process_status.into());
                    }
                    if let Some(target) = target {
                        self.set_current_target(Some(target), "已就绪");
                    } else {
                        self.sync_has_target();
                    }
                    if title == "安装" {
                        // 安装成功不弹窗，这是用户明确要求的交互方式。
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
                        self.set_current_target(Some(path), "已就绪");
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

    /// 将成功返回的服务器数据转换为 ListView 行。
    pub(super) fn update_server_rows(&mut self, servers: Vec<RemoteServer>) {
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

    /// 在不替换模型对象的前提下同步服务器模型。
    pub(super) fn set_server_rows(&mut self, rows: Vec<ServerRow>) {
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

    /// 将 core 的端口诊断结果映射为 Slint 行。
    pub(super) fn update_port_rows(&mut self, rows: Vec<CorePortStatusRow>) {
        let mapped = rows
            .into_iter()
            .map(|row| {
                let protocol = row.protocol.to_string();
                let port = i32::from(row.port);
                let (occupied, expected, pid, detail) = row
                    .conflict
                    .as_ref()
                    .map(|conflict| {
                        let prefix = if conflict.expected {
                            "目标进程"
                        } else {
                            "异常占用"
                        };
                        (
                            true,
                            conflict.expected,
                            i32::try_from(conflict.pid).unwrap_or(0),
                            format!("{prefix}：PID {} {}", conflict.pid, conflict.name),
                        )
                    })
                    .unwrap_or_else(|| (false, false, 0, "空闲".to_string()));
                PortRow {
                    occupied,
                    expected,
                    label: format!("{}/{}", row.protocol, row.port).into(),
                    detail: detail.into(),
                    protocol: protocol.into(),
                    port,
                    pid,
                }
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
}
