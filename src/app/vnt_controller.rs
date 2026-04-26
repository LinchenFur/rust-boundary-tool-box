//! AppController 联机会话控制和状态同步。

use super::*;

impl AppController {
    /// 启动本地并入的 VNT 核心，并将状态流式同步到 UI。
    pub(super) fn start_vnt(&mut self) {
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

    /// 请求 VNT 会话停止；最终清理结果会通过事件返回。
    pub(super) fn stop_vnt(&mut self) {
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

    /// 等待下一次 VNT 快照刷新时先给出即时反馈。
    pub(super) fn refresh_vnt_status_hint(&mut self) {
        if self.vnt_session.is_some() {
            self.ui.set_vnt_detail_text("等待联机核心刷新状态".into());
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
        self.set_vnt_peer_rows(snapshot.peers.into_iter().map(vnt_peer_to_row).collect());
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
}
