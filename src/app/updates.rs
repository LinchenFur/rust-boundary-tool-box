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

    /// 后台下载已经检查到的新版本 exe，并安排替换当前程序后重启。
    pub(super) fn start_update_download(&mut self, result: UpdateCheckResult) {
        if self.ui.get_update_checking() {
            return;
        }

        self.hide_app_dialog();
        self.ui.set_busy(true);
        self.ui.set_update_checking(true);
        self.ui.set_install_progress_cancelable(false);
        self.ui.set_install_progress_dialog_title(
            self.tr("下载更新", "Download Update", "更新をダウンロード")
                .into(),
        );
        self.ui.set_install_progress_visible(true);
        self.ui.set_install_progress_value(0.0);
        self.ui.set_install_progress_percent("0%".into());
        self.ui.set_install_progress_title(
            self.tr("准备下载更新", "Preparing update", "更新準備中")
                .into(),
        );
        self.set_install_progress_detail_text(self.tr(
            "正在准备从 GitHub Release 下载更新。",
            "Preparing to download the update from GitHub Release.",
            "GitHub Release から更新をダウンロードする準備をしています。",
        ));
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
        let script_dir = self.core.installer_home.join("update");
        let proxy_prefix = self.core.github_proxy_prefix();
        let progress_tx = tx.clone();
        let progress = Arc::new(move |progress: InstallProgress| {
            let _ = progress_tx.send(AppMessage::InstallProgress(progress));
        });
        thread::spawn(move || {
            let tag = result.latest_tag.clone();
            let download_progress = Arc::clone(&progress);
            match download_release_asset(
                &result,
                &runtime_dir,
                &fallback_dir,
                &proxy_prefix,
                download_progress,
            ) {
                Ok(path) => match schedule_self_replace_and_restart(&path, &script_dir, progress) {
                    Ok(()) => {
                        let _ = tx.send(AppMessage::UpdateRestartScheduled { tag });
                    }
                    Err(error) => {
                        let _ = tx.send(AppMessage::UpdateDownloadFailed(error.to_string()));
                    }
                },
                Err(error) => {
                    let _ = tx.send(AppMessage::UpdateDownloadFailed(error.to_string()));
                }
            }
        });
    }
}
