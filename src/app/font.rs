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
        self.show_app_dialog(
            self.tr("界面字体", "UI Font", "UI フォント"),
            self.tr(
                "未检测到 Maple Mono NF CN，正在自动下载并安装字体。\n字体包约 150 MB，请保持网络连接。安装完成后如果界面没有立刻切换字体，请重启工具箱。",
                "Maple Mono NF CN was not found, so the toolbox is downloading and installing it automatically.\nThe font package is about 150 MB. Keep the network connected. If the UI does not switch fonts immediately after installation, restart the toolbox.",
                "Maple Mono NF CN が見つからないため、自動でダウンロードしてインストールします。\nフォントパッケージは約 150 MB です。ネットワーク接続を維持してください。インストール後すぐに UI フォントが切り替わらない場合は、ツールボックスを再起動してください。",
            ),
            self.tr("知道了", "Got it", "了解"),
            "",
            false,
            false,
        );
        self.pending_dialog_action = PendingDialogAction::None;
        self.append_log(&format!(
            "[{}] 未检测到 Maple Mono NF CN，开始自动下载并安装。",
            core::now_text()
        ));

        let core = self.core.clone();
        let tx = self.tx.clone();
        thread::spawn(move || match core.install_ui_font() {
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
