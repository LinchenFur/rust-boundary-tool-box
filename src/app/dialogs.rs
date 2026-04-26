//! AppController 应用内弹窗和日志目录动作。

use super::*;

impl AppController {
    /// 显示非错误应用内弹窗。
    pub(super) fn show_info_dialog(&mut self, title: &str, text: &str) {
        self.show_app_dialog(title, text, "确定", "", false, false);
        self.pending_dialog_action = PendingDialogAction::None;
    }

    /// 显示错误应用内弹窗。
    pub(super) fn show_error_dialog(&mut self, title: &str, text: &str) {
        self.show_app_dialog(title, text, "确定", "", false, true);
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
            "选择游戏根目录",
            "输入或粘贴 Boundary 游戏根目录。可以填写游戏根目录，也可以填写 Binaries\\Win64 目录。",
            "应用路径",
            "取消",
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
        self.pending_dialog_action = PendingDialogAction::None;
    }

    /// 处理弹窗主按钮。
    pub(super) fn handle_dialog_primary(&mut self) {
        let action = std::mem::replace(&mut self.pending_dialog_action, PendingDialogAction::None);
        match action {
            PendingDialogAction::None => self.hide_app_dialog(),
            PendingDialogAction::ManualPathInput => self.confirm_manual_path_from_dialog(),
            PendingDialogAction::LaunchWithConflicts {
                target,
                keep_topmost,
                hotkey,
                conflicts,
            } => {
                self.hide_app_dialog();
                self.start_launch_with_conflicts(target, keep_topmost, hotkey, conflicts);
            }
        }
    }

    /// 处理弹窗取消动作，并应用取消侧的状态变化。
    pub(super) fn handle_dialog_secondary(&mut self) {
        if matches!(
            &self.pending_dialog_action,
            PendingDialogAction::LaunchWithConflicts { .. }
        ) {
            self.ui.set_status_text("已取消启动".into());
        }
        self.hide_app_dialog();
    }

    /// 校验并应用弹窗中输入的手动路径。
    pub(super) fn confirm_manual_path_from_dialog(&mut self) {
        let raw = self.ui.get_app_dialog_input_text().to_string();
        if raw.trim().is_empty() {
            self.ui.set_app_dialog_title("路径无效".into());
            self.ui.set_app_dialog_text("游戏根目录不能为空。".into());
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
                self.ui.set_detected_text("已手动设置游戏根目录".into());
                self.set_current_target(Some(path.clone()), "已就绪", true);
                self.append_log(&format!(
                    "[{}] 手动路径已设置：{}",
                    core::now_text(),
                    path.display()
                ));
            }
            Err(error) => {
                self.ui.set_app_dialog_title("路径无效".into());
                self.ui.set_app_dialog_text(error.to_string().into());
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
