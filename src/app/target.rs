//! AppController 目标目录、路径模式和快捷键设置。

use super::*;

impl AppController {
    /// 在 Steam 自动识别和手动路径模式之间切换。
    pub(super) fn set_mode(&mut self, mode: PathMode) {
        self.mode = mode;
        self.ui.set_auto_mode(matches!(mode, PathMode::Auto));
        self.refresh_target_from_mode(false);
    }

    /// 用户编辑输入框时更新手动路径状态。
    pub(super) fn on_manual_path_changed(&mut self, text: String) {
        self.ui.set_manual_path(text.clone().into());
        if matches!(self.mode, PathMode::Manual) {
            self.refresh_target_from_mode(false);
        }
    }

    /// 打开自定义路径输入弹窗，而不是系统原生文件夹选择器。
    pub(super) fn browse_path(&mut self) {
        if self.ui.get_busy() {
            return;
        }
        self.mode = PathMode::Manual;
        self.ui.set_auto_mode(false);
        let initial = if !self.ui.get_manual_path().is_empty() {
            self.ui.get_manual_path().to_string()
        } else {
            self.current_target
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default()
        };
        self.show_path_dialog(&initial);
    }

    /// 根据当前路径模式刷新目标目录。
    pub(super) fn refresh_target_from_mode(&mut self, initial: bool) {
        match self.mode {
            PathMode::Auto => match self.core.detect_steam_game_win64() {
                Ok((path, message)) => {
                    self.ui.set_detected_text(message.clone().into());
                    self.set_current_target(Some(path), "已就绪", true);
                    if !initial {
                        self.append_log(&format!("[{}] {}", core::now_text(), message));
                    }
                }
                Err(error) => {
                    let text = format!("自动识别失败：{}", error);
                    self.ui.set_detected_text(text.clone().into());
                    self.ui
                        .set_status_text("可手动选择路径或使用全盘扫描".into());
                    self.set_current_target(None, "可手动选择路径或使用全盘扫描", false);
                    if !initial {
                        self.append_log(&format!("[{}] 自动识别失败：{}", core::now_text(), error));
                    }
                }
            },
            PathMode::Manual => {
                let raw = self.ui.get_manual_path().to_string();
                if raw.trim().is_empty() {
                    self.set_current_target(None, "请选择游戏路径或使用全盘扫描", false);
                    return;
                }
                match self.core.normalize_selected_path(Path::new(raw.trim())) {
                    Ok(path) => {
                        self.set_current_target(Some(path.clone()), "已就绪", true);
                        self.append_log(&format!(
                            "[{}] 手动路径已解析：{}",
                            core::now_text(),
                            path.display()
                        ));
                    }
                    Err(error) => {
                        self.ui.set_status_text(error.to_string().into());
                        self.set_current_target(None, &error.to_string(), false);
                        self.append_log(&format!("[{}] 手动路径无效：{}", core::now_text(), error));
                    }
                }
            }
        }
    }

    /// 保存当前目标目录，并同步到 Slint 属性。
    pub(super) fn set_current_target(
        &mut self,
        path: Option<PathBuf>,
        status: &str,
        load_topmost: bool,
    ) {
        self.current_target = path;
        self.ui.set_target_text(
            self.current_target
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "未解析到有效的安装目录".to_string())
                .into(),
        );
        self.ui.set_status_text(status.into());
        if self.current_target.is_none() {
            self.ui.set_process_status_text("运行进程：未检测".into());
        }
        if load_topmost && let Some(target) = &self.current_target {
            let config = self.core.read_topmost_config(target);
            self.ui.set_keep_topmost(config.keep_topmost);
            self.ui.set_hotkey_text(config.hotkey.into());
        }
        self.sync_has_target();
    }

    /// 根据目标目录是否可用，同步 UI 的启用/禁用绑定。
    pub(super) fn sync_has_target(&self) {
        self.ui.set_has_target(self.current_target.is_some());
    }

    /// 将 Slint 键盘事件转换为规范化的全局快捷键字符串。
    pub(super) fn capture_hotkey(
        &mut self,
        text: String,
        control: bool,
        alt: bool,
        shift: bool,
        meta: bool,
    ) {
        if hotkey_capture_is_escape(&text) {
            self.ui.set_hotkey_listening(false);
            self.ui.set_status_text("快捷键设置已取消".into());
            return;
        }

        let Some(candidate) = hotkey_from_capture(&text, control, alt, shift, meta) else {
            self.ui.set_status_text("请继续按下主键".into());
            return;
        };

        match core::normalize_hotkey(&candidate) {
            Ok(normalized) => {
                self.ui.set_hotkey_text(normalized.clone().into());
                self.ui.set_hotkey_listening(false);
                self.ui
                    .set_status_text(format!("快捷键已设置：{normalized}").into());
            }
            Err(error) => {
                self.ui
                    .set_status_text(format!("快捷键无效：{}", error).into());
            }
        }
    }

    /// 根据当前模式返回已校验目标目录，或生成用户可读错误。
    pub(super) fn require_target(&mut self) -> Result<PathBuf> {
        match self.mode {
            PathMode::Auto => {
                let (path, _) = self.core.detect_steam_game_win64()?;
                self.set_current_target(Some(path.clone()), "已就绪", false);
                Ok(path)
            }
            PathMode::Manual => {
                let raw = self.ui.get_manual_path().to_string();
                if raw.trim().is_empty() {
                    bail!("请先选择游戏路径。");
                }
                let path = self.core.normalize_selected_path(Path::new(raw.trim()))?;
                self.set_current_target(Some(path.clone()), "已就绪", false);
                Ok(path)
            }
        }
    }
}
