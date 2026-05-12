//! AppController 应用内弹窗和日志目录动作。

use super::*;

const DIALOG_TEXT_COLUMNS: usize = 60;

pub(super) fn estimate_dialog_text_lines(text: &str, columns: usize) -> i32 {
    let columns = columns.max(1);
    let mut lines = 0usize;
    for physical_line in text.split('\n') {
        let chars = physical_line.chars().count().max(1);
        lines += chars.div_ceil(columns);
    }
    i32::try_from(lines.clamp(1, 240)).unwrap_or(240)
}

impl AppController {
    /// 显示非错误应用内弹窗。
    pub(super) fn show_info_dialog(&mut self, title: &str, text: &str) {
        self.show_app_dialog(title, text, self.tr("确定", "OK", "OK"), "", false, false);
        self.pending_dialog_action = PendingDialogAction::None;
    }

    /// 显示错误应用内弹窗。
    pub(super) fn show_error_dialog(&mut self, title: &str, text: &str) {
        self.show_app_dialog(title, text, self.tr("确定", "OK", "OK"), "", false, true);
        self.pending_dialog_action = PendingDialogAction::None;
    }

    /// 显示应用内确认弹窗，并记录确认后要执行的动作。
    pub(super) fn show_confirm_dialog(
        &mut self,
        title: &str,
        text: &str,
        primary: &str,
        secondary: &str,
        action: PendingDialogAction,
    ) {
        self.show_app_dialog(title, text, primary, secondary, true, false);
        self.pending_dialog_action = action;
    }

    /// 显示应用内手动路径输入弹窗。
    pub(super) fn show_path_dialog(&mut self, initial: &str) {
        self.show_app_dialog(
            self.tr("选择游戏根目录", "Select Game Root", "ゲームルートを選択"),
            self.tr(
                "输入或粘贴 Boundary 游戏根目录。可以填写游戏根目录，也可以填写 Binaries\\Win64 目录。",
                "Enter or paste the Boundary game root. You can use the game root or the Binaries\\Win64 folder.",
                "Boundary のゲームルートを入力または貼り付けてください。ゲームルートまたは Binaries\\Win64 フォルダーを指定できます。",
            ),
            self.tr("应用路径", "Apply Path", "パスを適用"),
            self.tr("取消", "Cancel", "キャンセル"),
            false,
            false,
        );
        self.ui.set_app_dialog_input(true);
        self.ui.set_app_dialog_input_text(initial.into());
        self.pending_dialog_action = PendingDialogAction::ManualPathInput;
    }

    /// 信息、错误、确认和输入流程共用的弹窗状态设置函数。
    pub(super) fn show_app_dialog(
        &mut self,
        title: &str,
        text: &str,
        primary: &str,
        secondary: &str,
        confirm: bool,
        error: bool,
    ) {
        self.ui.set_app_dialog_title(title.into());
        self.ui.set_app_dialog_text(text.into());
        self.ui
            .set_app_dialog_text_lines(estimate_dialog_text_lines(text, DIALOG_TEXT_COLUMNS));
        self.ui.set_app_dialog_primary_text(primary.into());
        self.ui.set_app_dialog_secondary_text(secondary.into());
        self.ui.set_app_dialog_confirm(confirm);
        self.ui.set_app_dialog_input(false);
        self.ui.set_app_dialog_error(error);
        self.ui.set_show_app_dialog(true);
    }

    /// 清空弹窗状态和所有待执行动作。
    pub(super) fn hide_app_dialog(&mut self) {
        self.ui.set_show_app_dialog(false);
        self.ui.set_app_dialog_confirm(false);
        self.ui.set_app_dialog_input(false);
        self.ui.set_app_dialog_error(false);
        self.ui.set_app_dialog_text_lines(1);
        self.pending_dialog_action = PendingDialogAction::None;
    }

    /// 处理弹窗主按钮。
    pub(super) fn handle_dialog_primary(&mut self) {
        let action = std::mem::replace(&mut self.pending_dialog_action, PendingDialogAction::None);
        match action {
            PendingDialogAction::None => self.hide_app_dialog(),
            PendingDialogAction::ManualPathInput => self.confirm_manual_path_from_dialog(),
            PendingDialogAction::DownloadUpdate { result } => self.start_update_download(result),
            PendingDialogAction::CloseApplication => self.close_window_now(),
            PendingDialogAction::LaunchWithConflicts {
                target,
                mode,
                conflicts,
            } => {
                self.hide_app_dialog();
                self.start_launch_with_conflicts(target, mode, conflicts);
            }
        }
    }

    /// 处理弹窗取消动作，并应用取消侧的状态变化。
    pub(super) fn handle_dialog_secondary(&mut self) {
        if let PendingDialogAction::LaunchWithConflicts { mode, .. } = &self.pending_dialog_action {
            self.ui.set_status_text(
                format!(
                    "{} {}",
                    self.tr("已取消", "Cancelled", "キャンセル済み"),
                    mode.display_name()
                )
                .into(),
            );
        }
        self.hide_app_dialog();
    }

    /// 校验并应用弹窗中输入的手动路径。
    pub(super) fn confirm_manual_path_from_dialog(&mut self) {
        let raw = self.ui.get_app_dialog_input_text().to_string();
        if raw.trim().is_empty() {
            self.ui
                .set_app_dialog_title(self.tr("路径无效", "Invalid Path", "無効なパス").into());
            self.ui.set_app_dialog_text(
                self.tr(
                    "游戏根目录不能为空。",
                    "Game root cannot be empty.",
                    "ゲームルートは空にできません。",
                )
                .into(),
            );
            self.ui
                .set_app_dialog_text_lines(estimate_dialog_text_lines(
                    self.tr(
                        "游戏根目录不能为空。",
                        "Game root cannot be empty.",
                        "ゲームルートは空にできません。",
                    ),
                    DIALOG_TEXT_COLUMNS,
                ));
            self.ui.set_app_dialog_error(true);
            self.pending_dialog_action = PendingDialogAction::ManualPathInput;
            return;
        }

        match self.core.normalize_selected_path(Path::new(raw.trim())) {
            Ok(path) => {
                self.hide_app_dialog();
                self.mode = PathMode::Manual;
                self.ui.set_auto_mode(false);
                self.ui.set_manual_path(path.display().to_string().into());
                self.ui.set_detected_text(
                    self.tr(
                        "已手动设置游戏根目录",
                        "Game root set manually",
                        "ゲームルートを手動で設定しました",
                    )
                    .into(),
                );
                self.set_current_target(Some(path.clone()), self.tr("已就绪", "Ready", "準備完了"));
                self.append_log(&format!(
                    "[{}] 手动路径已设置：{}",
                    core::now_text(),
                    path.display()
                ));
            }
            Err(error) => {
                self.ui
                    .set_app_dialog_title(self.tr("路径无效", "Invalid Path", "無効なパス").into());
                let error = error.to_string();
                self.ui.set_app_dialog_text(error.clone().into());
                self.ui
                    .set_app_dialog_text_lines(estimate_dialog_text_lines(
                        &error,
                        DIALOG_TEXT_COLUMNS,
                    ));
                self.ui.set_app_dialog_error(true);
                self.pending_dialog_action = PendingDialogAction::ManualPathInput;
            }
        }
    }

    /// 使用资源管理器打开日志目录。
    pub(super) fn open_logs_dir(&self) {
        let _ = std::process::Command::new("explorer")
            .arg(self.core.installer_home.join("logs"))
            .spawn();
    }
}
