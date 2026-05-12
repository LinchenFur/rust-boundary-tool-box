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

    /// 后台下载已经检查到的新版本 exe。
    pub(super) fn start_update_download(&mut self, result: UpdateCheckResult) {
        if self.ui.get_update_checking() {
            return;
        }

        self.hide_app_dialog();
        self.ui.set_update_checking(true);
        self.ui.set_update_status_text(
            self.tr(
                "更新：下载中...",
                "Update: downloading...",
                "更新: ダウンロード中...",
            )
            .into(),
        );
        let tx = self.tx.clone();
        let runtime_dir = self.core.runtime_dir.clone();
        let fallback_dir = self.core.installer_home.join("downloads");
        let proxy_prefix = self.core.github_proxy_prefix();
        thread::spawn(move || {
            let tag = result.latest_tag.clone();
            match download_release_asset(&result, &runtime_dir, &fallback_dir, &proxy_prefix) {
                Ok(path) => {
                    let _ = tx.send(AppMessage::UpdateDownloadFinished { tag, path });
                }
                Err(error) => {
                    let _ = tx.send(AppMessage::UpdateDownloadFailed(error.to_string()));
                }
            }
        });
    }
}
