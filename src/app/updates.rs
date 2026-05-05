//! AppController 更新检查动作。

use super::*;

impl AppController {
    /// 启动后台更新检查；automatic=true 时只在发现新版本时弹窗。
    pub(super) fn start_update_check(&mut self, automatic: bool) {
        if self.ui.get_update_checking() {
            return;
        }

        self.ui.set_update_checking(true);
        self.ui.set_update_status_text(
            self.tr("更新：检查中...", "Update: checking...", "更新: 確認中...")
                .into(),
        );
        let tx = self.tx.clone();
        thread::spawn(move || match check_latest_release() {
            Ok(result) => {
                let _ = tx.send(AppMessage::UpdateCheckFinished { result, automatic });
            }
            Err(error) => {
                let _ = tx.send(AppMessage::UpdateCheckFailed {
                    error: error.to_string(),
                    automatic,
                });
            }
        });
    }
}
