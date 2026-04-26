//! 进程和端口诊断文本整理。

use crate::core::{self, PortConflict, RuntimeSnapshot, format_port_conflicts};

pub(crate) fn runtime_snapshot_has_any(snapshot: &RuntimeSnapshot) -> bool {
    !snapshot.game.is_empty()
        || !snapshot.wrapper.is_empty()
        || !snapshot.server.is_empty()
        || !snapshot.watcher.is_empty()
}

pub(crate) fn format_process_detection_message(
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
