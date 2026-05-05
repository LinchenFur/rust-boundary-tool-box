//! 进程和端口诊断文本整理。

use crate::app::i18n;
use crate::core::{PortConflict, RuntimeProcess, RuntimeSnapshot, format_port_conflicts};

pub(crate) fn runtime_snapshot_has_any(snapshot: &RuntimeSnapshot) -> bool {
    !snapshot.game.is_empty() || !snapshot.wrapper.is_empty() || !snapshot.server.is_empty()
}

pub(crate) fn format_process_detection_message(
    snapshot: &RuntimeSnapshot,
    conflicts: &[PortConflict],
    language: i32,
) -> String {
    let mut parts = vec![format!(
        "{}{}",
        i18n::tr(
            language,
            "相关运行进程：",
            "Related runtime processes: ",
            "関連実行プロセス: "
        ),
        summarize_runtime_processes(snapshot, language)
    )];
    if conflicts.is_empty() {
        parts.push(
            i18n::tr(
                language,
                "端口占用：未发现。",
                "Port usage: none found.",
                "ポート使用: 見つかりません。",
            )
            .to_string(),
        );
    } else {
        parts.push(format!(
            "{}\n{}",
            i18n::tr(language, "端口占用：", "Port usage:", "ポート使用:"),
            format_port_conflicts(conflicts)
        ));
    }
    parts.join("\n\n")
}

pub(crate) fn summarize_runtime_processes(snapshot: &RuntimeSnapshot, language: i32) -> String {
    [
        (i18n::tr(language, "游戏", "Game", "ゲーム"), &snapshot.game),
        (
            i18n::tr(
                language,
                "服务包装器",
                "Service wrapper",
                "サービスラッパー",
            ),
            &snapshot.wrapper,
        ),
        (
            i18n::tr(language, "登录服务器", "Login server", "ログインサーバー"),
            &snapshot.server,
        ),
    ]
    .into_iter()
    .map(|(label, items)| {
        if items.is_empty() {
            format!("{} {}", label, i18n::tr(language, "0 个", "0", "0 件"))
        } else {
            let details = items
                .iter()
                .map(format_runtime_process)
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "{} {} {} ({})",
                label,
                items.len(),
                i18n::tr(language, "个", "", "件"),
                details
            )
        }
    })
    .collect::<Vec<_>>()
    .join(i18n::tr(language, "；", "; ", "；"))
}

fn format_runtime_process(process: &RuntimeProcess) -> String {
    let name = if process.name.trim().is_empty() {
        "unknown"
    } else {
        process.name.trim()
    };
    if process.exe.trim().is_empty() && process.cmd.trim().is_empty() {
        format!("{} PID {}", name, process.pid)
    } else {
        let detail = if process.exe.trim().is_empty() {
            process.cmd.trim()
        } else {
            process.exe.trim()
        };
        format!("{} PID {} @ {}", name, process.pid, shorten(detail))
    }
}

fn shorten(value: &str) -> String {
    if value.chars().count() <= 96 {
        return value.to_string();
    }
    let mut shortened = value.chars().take(93).collect::<String>();
    shortened.push_str("...");
    shortened
}
