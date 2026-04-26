//! VNT 状态快照到 Slint 行模型的转换。

use crate::vnt_platform::{self, VntPeer};
use crate::{AppWindow, VntPeerRow};

/// 将默认未连接 VNT 状态应用到 Slint 属性。
pub(crate) fn apply_vnt_idle_to_ui(ui: &AppWindow) {
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

/// 在 VNT 运行前展示的默认节点行。
pub(crate) fn vnt_placeholder_rows() -> Vec<VntPeerRow> {
    vnt_platform::idle_snapshot()
        .peers
        .into_iter()
        .map(vnt_peer_to_row)
        .collect()
}

/// 将 VNT 节点快照映射为 Slint 行。
pub(crate) fn vnt_peer_to_row(peer: VntPeer) -> VntPeerRow {
    VntPeerRow {
        name: peer.name.into(),
        address: peer.address.into(),
        detail: peer.detail.into(),
        online: peer.online,
    }
}
