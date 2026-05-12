//! 关闭窗口前的运行中游戏保护。

use super::*;

impl AppController {
    /// 统一处理窗口关闭请求；返回 true 表示可以立即关闭窗口。
    pub(super) fn request_window_close(&mut self) -> bool {
        if let Some(text) = self.running_game_close_warning_text() {
            self.show_confirm_dialog(
                self.tr("游戏仍在运行", "Game Still Running", "ゲーム実行中"),
                &text,
                self.tr("仍然关闭", "Close Anyway", "そのまま閉じる"),
                self.tr("取消", "Cancel", "キャンセル"),
                PendingDialogAction::CloseApplication,
            );
            return false;
        }
        true
    }

    /// 用户确认后真正退出 UI。
    pub(super) fn close_window_now(&mut self) {
        self.hide_app_dialog();
        self.stop_background.store(true, Ordering::Relaxed);
        let _ = self.ui.hide();
    }

    /// 只在当前目标目录下检测到游戏进程时生成提醒文本，避免误判其它安装目录。
    fn running_game_close_warning_text(&mut self) -> Option<String> {
        let target = self.current_target.clone()?;
        match self.core.collect_runtime_processes(&target) {
            Ok(snapshot) if !snapshot.game.is_empty() => {
                let summary = summarize_runtime_processes(&snapshot, self.language());
                Some(format!(
                    "{}\n\n{}\n\n{}",
                    self.tr(
                        "检测到 Boundary 游戏仍在运行。",
                        "Boundary is still running.",
                        "Boundary がまだ実行中です。",
                    ),
                    summary,
                    self.tr(
                        "关闭工具箱不会自动关闭游戏或后台服务，确定要退出吗？",
                        "Closing the toolbox will not stop the game or background services. Exit anyway?",
                        "ツールボックスを閉じてもゲームやバックグラウンドサービスは停止しません。終了しますか？",
                    )
                ))
            }
            Ok(_) => None,
            Err(error) => {
                self.append_log(&format!(
                    "[{}] 关闭前检测游戏进程失败：{}",
                    core::now_text(),
                    error
                ));
                None
            }
        }
    }
}
