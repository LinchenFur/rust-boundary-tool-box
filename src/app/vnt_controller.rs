//! AppController 联机会话控制和状态同步。

use super::*;

impl AppController {
    /// 将用户输入的 VNT 服务器地址加入下拉框，并立即切换到这个地址。
    pub(super) fn add_vnt_server_option(&mut self, address: String) {
        let address = address.trim().to_string();
        if address.is_empty() {
            return;
        }

        let exists = (0..self.vnt_server_option_model.row_count()).any(|index| {
            self.vnt_server_option_model
                .row_data(index)
                .map(|item| item.as_str().eq_ignore_ascii_case(&address))
                .unwrap_or(false)
        });
        if !exists {
            self.vnt_server_option_model.push(address.clone().into());
        }

        self.ui.set_vnt_server_text(address.into());
        self.ui.set_vnt_new_server_text("".into());
        self.save_app_prefs();
    }

    /// 启动本地并入的 VNT 核心，并将状态流式同步到 UI。
    pub(super) fn start_vnt(&mut self) {
        if self.vnt_session.is_some() || self.ui.get_vnt_busy() {
            return;
        }
        self.save_app_prefs();

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
        self.ui
            .set_vnt_status_text(self.tr("启动中", "Starting", "起動中").into());
        self.ui.set_vnt_detail_text(
            self.tr(
                "正在启动联机平台",
                "Starting the network platform",
                "ネットワーク基盤を起動中",
            )
            .into(),
        );
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
                self.ui
                    .set_vnt_status_text(self.tr("启动失败", "Start failed", "起動失敗").into());
                self.ui.set_vnt_detail_text(error.to_string().into());
                self.append_log(&format!("[{}] VNT 启动失败：{}", core::now_text(), error));
                self.show_error_dialog(
                    self.tr("联机", "Network", "ネットワーク"),
                    &error.to_string(),
                );
            }
        }
    }

    /// 请求 VNT 会话停止；最终清理结果会通过事件返回。
    pub(super) fn stop_vnt(&mut self) {
        if let Some(session) = self.vnt_session.as_mut() {
            session.stop();
            self.ui.set_vnt_busy(true);
            self.ui
                .set_vnt_status_text(self.tr("停止中", "Stopping", "停止中").into());
            self.ui.set_vnt_detail_text(
                self.tr(
                    "正在关闭联机平台",
                    "Stopping the network platform",
                    "ネットワーク基盤を停止中",
                )
                .into(),
            );
            self.append_log(&format!("[{}] 正在停止 VNT 联机", core::now_text()));
        } else {
            self.apply_vnt_snapshot(localized_vnt_idle_snapshot(self.language()));
        }
    }

    /// 等待下一次 VNT 快照刷新时先给出即时反馈。
    pub(super) fn refresh_vnt_status_hint(&mut self) {
        if self.vnt_session.is_some() {
            self.ui.set_vnt_detail_text(
                self.tr(
                    "等待联机核心刷新状态",
                    "Waiting for the network core to refresh",
                    "ネットワークコアの状態更新待ち",
                )
                .into(),
            );
        }
    }

    /// 应用 VNT 工作线程发出的生命周期事件。
    pub(super) fn apply_vnt_event(&mut self, event: VntEvent) {
        match event {
            VntEvent::Snapshot(snapshot) => self.apply_vnt_snapshot(snapshot),
            VntEvent::Failed(error) => {
                self.vnt_session = None;
                self.ui.set_vnt_busy(false);
                self.ui.set_vnt_running(false);
                self.ui
                    .set_vnt_status_text(self.tr("启动失败", "Start failed", "起動失敗").into());
                self.ui.set_vnt_detail_text(error.clone().into());
                self.set_vnt_server_rows(vnt_server_placeholder_rows(self.language()));
                self.set_vnt_peer_rows(vnt_placeholder_rows(self.language()));
                self.append_log(&format!("[{}] VNT 异常：{}", core::now_text(), error));
                self.show_error_dialog(self.tr("联机", "Network", "ネットワーク"), &error);
            }
            VntEvent::Stopped(reason) => {
                self.vnt_session = None;
                let mut snapshot = localized_vnt_idle_snapshot(self.language());
                snapshot.detail = reason.clone();
                self.apply_vnt_snapshot(snapshot);
                self.append_log(&format!("[{}] VNT 已停止：{}", core::now_text(), reason));
            }
        }
    }

    /// 将 VNT 快照同步到 Slint 属性和节点模型。
    pub(super) fn apply_vnt_snapshot(&mut self, snapshot: vnt_platform::VntSnapshot) {
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
        self.set_vnt_server_rows(
            snapshot
                .servers
                .into_iter()
                .map(vnt_server_to_row)
                .collect(),
        );
        self.set_vnt_peer_rows(snapshot.peers.into_iter().map(vnt_peer_to_row).collect());
    }

    /// 为 ListView 原地同步 VNT 服务器模型。
    pub(super) fn set_vnt_server_rows(&mut self, rows: Vec<VntServerRow>) {
        while self.vnt_server_model.row_count() > rows.len() {
            let _ = self
                .vnt_server_model
                .remove(self.vnt_server_model.row_count() - 1);
        }
        for (index, row) in rows.into_iter().enumerate() {
            if index < self.vnt_server_model.row_count() {
                self.vnt_server_model.set_row_data(index, row);
            } else {
                self.vnt_server_model.push(row);
            }
        }
    }

    /// 为 ListView 原地同步节点模型。
    pub(super) fn set_vnt_peer_rows(&mut self, rows: Vec<VntPeerRow>) {
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

    /// 从当前 UI 收集联机页偏好设置。
    pub(super) fn current_vnt_prefs(&self) -> VntPrefs {
        let server_text = self.ui.get_vnt_server_text().trim().to_string();
        let mut server_options = Vec::new();
        for index in 0..self.vnt_server_option_model.row_count() {
            if let Some(option) = self.vnt_server_option_model.row_data(index) {
                let option = option.trim();
                if option.is_empty() {
                    continue;
                }
                if !server_options
                    .iter()
                    .any(|existing: &String| existing.eq_ignore_ascii_case(option))
                {
                    server_options.push(option.to_string());
                }
            }
        }
        if !server_text.is_empty()
            && !server_options
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&server_text))
        {
            server_options.push(server_text.clone());
        }

        VntPrefs {
            server_text,
            server_options,
            network_code: self.ui.get_vnt_network_code().trim().to_string(),
            password: self.ui.get_vnt_password().to_string(),
            no_tun: self.ui.get_vnt_no_tun(),
            compress: self.ui.get_vnt_compress(),
            rtx: self.ui.get_vnt_rtx(),
        }
    }

    /// 更新 GitHub 下载代理前缀，并立即保存到应用配置。
    pub(super) fn set_github_proxy_prefix(&mut self, proxy: String) {
        let normalized = core::normalize_github_proxy_prefix(&proxy);
        self.core.set_github_proxy_prefix(&normalized);
        self.app_prefs.github_proxy_prefix = normalized;
        self.sync_github_proxy_current_selection();
        self.save_app_prefs();
    }

    /// 从代理选择弹窗选中节点后写回设置并关闭弹窗。
    pub(super) fn select_github_proxy_from_dialog(&mut self, proxy: String) {
        let normalized = core::normalize_github_proxy_prefix(&proxy);
        self.ui.set_github_proxy_text(normalized.clone().into());
        self.set_github_proxy_prefix(normalized);
        self.ui.set_show_github_proxy_dialog(false);
    }

    /// 保存应用级偏好设置；失败只写日志，不打断用户操作。
    pub(super) fn save_app_prefs(&mut self) {
        self.app_prefs.vnt = self.current_vnt_prefs();
        self.app_prefs.github_proxy_prefix =
            core::normalize_github_proxy_prefix(&self.ui.get_github_proxy_text());
        self.core
            .set_github_proxy_prefix(&self.app_prefs.github_proxy_prefix);
        if let Err(error) = self.app_prefs.save(&self.core.installer_home) {
            self.append_log(&format!(
                "[{}] 应用配置保存失败：{}",
                core::now_text(),
                error
            ));
        }
    }
}
