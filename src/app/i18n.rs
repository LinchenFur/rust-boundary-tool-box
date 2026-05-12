//! Minimal application-side i18n helpers for Rust-owned UI strings.

use super::*;
#[cfg(not(windows))]
use std::env;
#[cfg(windows)]
use windows::Win32::Globalization::GetUserDefaultUILanguage;

pub(crate) const LANGUAGE_AUTO: i32 = -1;
pub(crate) const LANGUAGE_ZH: i32 = 0;
pub(crate) const LANGUAGE_EN: i32 = 1;
pub(crate) const LANGUAGE_JA: i32 = 2;

pub(crate) fn normalize_language_preference(language: i32) -> i32 {
    match language {
        LANGUAGE_AUTO | LANGUAGE_ZH | LANGUAGE_EN | LANGUAGE_JA => language,
        _ => LANGUAGE_AUTO,
    }
}

pub(crate) fn normalize_language(language: i32) -> i32 {
    match language {
        LANGUAGE_EN | LANGUAGE_JA => language,
        _ => LANGUAGE_ZH,
    }
}

pub(crate) fn resolve_language(preference: i32) -> i32 {
    match normalize_language_preference(preference) {
        LANGUAGE_AUTO => detect_system_language(),
        language => normalize_language(language),
    }
}

pub(crate) fn detect_system_language() -> i32 {
    detect_system_language_impl()
}

#[cfg(windows)]
fn detect_system_language_impl() -> i32 {
    // LANGID 的低 10 位是 primary language id，可直接区分中/英/日。
    let lang_id = unsafe { GetUserDefaultUILanguage() };
    language_from_windows_lang_id(lang_id)
}

#[cfg(not(windows))]
fn detect_system_language_impl() -> i32 {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|key| env::var(key).ok())
        .as_deref()
        .map(language_from_locale_text)
        .unwrap_or(LANGUAGE_EN)
}

fn language_from_windows_lang_id(lang_id: u16) -> i32 {
    match lang_id & 0x03ff {
        0x04 => LANGUAGE_ZH,
        0x11 => LANGUAGE_JA,
        0x09 => LANGUAGE_EN,
        _ => LANGUAGE_EN,
    }
}

#[cfg_attr(windows, allow(dead_code))]
fn language_from_locale_text(locale: &str) -> i32 {
    let lower = locale.to_ascii_lowercase();
    if lower.starts_with("zh") {
        LANGUAGE_ZH
    } else if lower.starts_with("ja") {
        LANGUAGE_JA
    } else {
        LANGUAGE_EN
    }
}

pub(crate) fn tr(
    language: i32,
    zh: &'static str,
    en: &'static str,
    ja: &'static str,
) -> &'static str {
    match normalize_language(language) {
        LANGUAGE_EN => en,
        LANGUAGE_JA => ja,
        _ => zh,
    }
}

impl AppController {
    pub(super) fn language(&self) -> i32 {
        resolve_language(self.app_prefs.language)
    }

    pub(super) fn tr(&self, zh: &'static str, en: &'static str, ja: &'static str) -> &'static str {
        tr(self.language(), zh, en, ja)
    }

    pub(super) fn set_language(&mut self, language: i32) {
        self.app_prefs.language = normalize_language_preference(language);
        let language = self.language();
        self.sync_slint_language(language);
        self.apply_language_defaults();
        self.save_app_prefs();
    }

    pub(super) fn sync_slint_language(&self, language: i32) {
        self.ui.set_language(language);
        self.ui.set_language_mode(self.app_prefs.language);
    }

    pub(super) fn localize_action_title(&self, title: &str) -> String {
        match title {
            "安装" => self.tr("安装", "Install", "インストール").to_string(),
            "卸载" => self.tr("卸载", "Uninstall", "アンインストール").to_string(),
            "进程检测" => self
                .tr("进程检测", "Process Detection", "プロセス検出")
                .to_string(),
            "关闭端口进程" => self
                .tr("关闭端口进程", "Stop Port Process", "ポートプロセス停止")
                .to_string(),
            "关闭所有进程" => self
                .tr(
                    "关闭所有进程",
                    "Stop All Processes",
                    "すべてのプロセスを停止",
                )
                .to_string(),
            "字体安装" => self
                .tr("字体安装", "Font Install", "フォントインストール")
                .to_string(),
            value if value.starts_with("启动 ") => {
                value.replacen("启动", self.tr("启动", "Launch", "起動"), 1)
            }
            value => value.to_string(),
        }
    }

    pub(super) fn localize_action_status(&self, status: &str) -> String {
        match status {
            "完成" => self.tr("完成", "Done", "完了").to_string(),
            "已取消" => self.tr("已取消", "Cancelled", "キャンセル済み").to_string(),
            "执行失败" => self.tr("执行失败", "Failed", "失敗").to_string(),
            "字体安装完成" => self
                .tr(
                    "字体安装完成",
                    "Font install complete",
                    "フォントインストール完了",
                )
                .to_string(),
            "字体安装失败" => self
                .tr(
                    "字体安装失败",
                    "Font install failed",
                    "フォントインストール失敗",
                )
                .to_string(),
            value => value.to_string(),
        }
    }

    fn apply_language_defaults(&mut self) {
        let language = self.language();
        if !self.ui.get_show_app_dialog() {
            self.ui
                .set_app_dialog_primary_text(tr(language, "确定", "OK", "OK").into());
            self.ui
                .set_app_dialog_secondary_text(tr(language, "取消", "Cancel", "キャンセル").into());
        }

        if self.current_target.is_none() {
            self.ui.set_target_text(
                tr(
                    language,
                    "未解析到有效的安装目录",
                    "No valid install path resolved",
                    "有効なインストール先が見つかりません",
                )
                .into(),
            );
            self.ui.set_process_status_text(
                tr(
                    language,
                    "运行进程：未检测",
                    "Runtime processes: not checked",
                    "実行中プロセス: 未確認",
                )
                .into(),
            );
        }

        if !self.ui.get_vnt_running() && !self.ui.get_vnt_busy() {
            apply_vnt_idle_to_ui(&self.ui, language);
            self.set_vnt_server_rows(vnt_server_placeholder_rows(language));
            self.set_vnt_peer_rows(vnt_placeholder_rows(language));
        }

        self.sync_github_proxy_current_selection();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_auto_from_windows_lang_id() {
        assert_eq!(language_from_windows_lang_id(0x0804), LANGUAGE_ZH);
        assert_eq!(language_from_windows_lang_id(0x0411), LANGUAGE_JA);
        assert_eq!(language_from_windows_lang_id(0x0409), LANGUAGE_EN);
        assert_eq!(language_from_windows_lang_id(0x0419), LANGUAGE_EN);
    }

    #[test]
    fn resolves_locale_text() {
        assert_eq!(language_from_locale_text("zh_CN.UTF-8"), LANGUAGE_ZH);
        assert_eq!(language_from_locale_text("ja-JP"), LANGUAGE_JA);
        assert_eq!(language_from_locale_text("en_US.UTF-8"), LANGUAGE_EN);
        assert_eq!(language_from_locale_text("ru_RU.UTF-8"), LANGUAGE_EN);
    }

    #[test]
    fn keeps_auto_as_preference_only() {
        assert_eq!(normalize_language_preference(LANGUAGE_AUTO), LANGUAGE_AUTO);
        assert_eq!(normalize_language_preference(99), LANGUAGE_AUTO);
        assert_eq!(normalize_language(LANGUAGE_AUTO), LANGUAGE_ZH);
    }
}
