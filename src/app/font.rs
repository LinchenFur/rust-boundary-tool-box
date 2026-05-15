//! AppController UI 字体自动安装流程。

use super::*;

impl AppController {
    /// 启动时检查 UI 字体，缺失时提示并在后台自动安装。
    pub(super) fn start_ui_font_check(&mut self) -> bool {
        match self.core.ui_font_installed() {
            Ok(true) => return false,
            Ok(false) => {}
            Err(error) => {
                self.append_log(&format!("[{}] 字体检查失败：{}", core::now_text(), error));
                return false;
            }
        }

        self.ui.set_status_text(
            self.tr(
                "正在安装界面字体...",
                "Installing UI font...",
                "UI フォントをインストール中...",
            )
            .into(),
        );
        self.ui.set_busy(true);
        self.ui.set_install_progress_cancelable(false);
        self.ui.set_install_progress_dialog_title(
            self.tr("界面字体", "UI Font", "UI フォント").into(),
        );
        self.ui.set_install_progress_visible(true);
        self.ui.set_install_progress_value(0.0);
        self.ui.set_install_progress_percent("0%".into());
        self.ui.set_install_progress_title(
            self.tr("准备字体", "Preparing font", "フォント準備中")
                .into(),
        );
        self.set_install_progress_detail_text(self.tr(
            "未检测到 Maple Mono CN，正在自动下载并安装字体。",
            "Maple Mono CN was not found. Downloading and installing it automatically.",
            "Maple Mono CN が見つからないため、自動でダウンロードしてインストールします。",
        ));
        self.pending_dialog_action = PendingDialogAction::None;
        self.append_log(&format!(
            "[{}] 未检测到 Maple Mono CN，开始自动下载并安装。",
            core::now_text()
        ));

        let core = self.core.clone();
        let tx = self.tx.clone();
        let progress_tx = tx.clone();
        let progress = Arc::new(move |progress: InstallProgress| {
            let _ = progress_tx.send(AppMessage::InstallProgress(progress));
        });
        thread::spawn(move || match core.install_ui_font_with_progress(progress) {
            Ok(dialog) => {
                let _ = tx.send(AppMessage::ActionFinished {
                    title: "字体安装".to_string(),
                    status: "字体安装完成".to_string(),
                    dialog,
                    process_status: None,
                    target: None,
                });
            }
            Err(error) => {
                let _ = tx.send(AppMessage::ActionFailed {
                    title: "字体安装".to_string(),
                    status: "字体安装失败".to_string(),
                    error: error.to_string(),
                });
            }
        });
        true
    }
}
