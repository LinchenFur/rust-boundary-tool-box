//! 系统环境检查的 UI 调度和文本格式化。

use super::*;
use crate::core::{FirewallCheck, NvidiaDriverCheck};

impl AppController {
    /// 后台检查显卡驱动和 Windows 防火墙，避免阻塞 Slint UI 线程。
    pub(super) fn start_system_check(&mut self) {
        self.ui.set_system_status_text(
            self.tr(
                "系统检查：检测中...",
                "System check: checking...",
                "システムチェック: 確認中...",
            )
            .into(),
        );
        self.ui.set_system_status_warning(false);

        let core = self.core.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let report = core.check_system_status();
            let _ = tx.send(AppMessage::SystemCheckFinished(report));
        });
    }

    /// 缓存系统检查结果，并刷新当前语言下的 UI 文本。
    pub(super) fn apply_system_check_report(&mut self, report: SystemCheckReport) {
        self.system_report = Some(report);
        self.refresh_system_status_text();
        self.append_log(&format!(
            "[{}] 系统检查完成：{}",
            core::now_text(),
            self.ui.get_system_status_text()
        ));
    }

    /// 按当前语言重新渲染系统检查状态。
    pub(super) fn refresh_system_status_text(&self) {
        if let Some(report) = &self.system_report {
            self.ui
                .set_system_status_text(format_system_check_report(report, self.language()).into());
            self.ui.set_system_status_warning(report.has_warning());
        } else {
            self.ui.set_system_status_text(
                self.tr(
                    "系统检查：未检测",
                    "System check: not checked",
                    "システムチェック: 未確認",
                )
                .into(),
            );
            self.ui.set_system_status_warning(false);
        }
    }
}

fn format_system_check_report(report: &SystemCheckReport, language: i32) -> String {
    format!(
        "{}\n{}",
        format_nvidia_check(&report.nvidia, language),
        format_firewall_check(&report.firewall, language)
    )
}

fn format_nvidia_check(check: &NvidiaDriverCheck, language: i32) -> String {
    match check {
        NvidiaDriverCheck::NotDetected => i18n::tr(
            language,
            "NVIDIA 驱动：未检测到 N 卡",
            "NVIDIA driver: no NVIDIA GPU detected",
            "NVIDIA ドライバー: NVIDIA GPU 未検出",
        )
        .to_string(),
        NvidiaDriverCheck::Unknown(error) => format!(
            "{}{error}",
            i18n::tr(
                language,
                "NVIDIA 驱动：无法确认，",
                "NVIDIA driver: unknown, ",
                "NVIDIA ドライバー: 確認不可、",
            )
        ),
        NvidiaDriverCheck::Ok { public_version, .. } => format!(
            "{}{public_version}{}",
            i18n::tr(
                language,
                "NVIDIA 驱动：",
                "NVIDIA driver: ",
                "NVIDIA ドライバー: ",
            ),
            i18n::tr(
                language,
                "，符合 531+ 建议",
                ", meets the 531+ recommendation",
                "、531+ 推奨を満たしています",
            )
        ),
        NvidiaDriverCheck::Outdated { public_version, .. } => format!(
            "{}{public_version}{}",
            i18n::tr(
                language,
                "NVIDIA 驱动：",
                "NVIDIA driver: ",
                "NVIDIA ドライバー: ",
            ),
            i18n::tr(
                language,
                "，低于建议版本 531+，建议更新",
                ", below the 531+ recommendation; update recommended",
                "、531+ 推奨未満のため更新推奨",
            )
        ),
    }
}

fn format_firewall_check(check: &FirewallCheck, language: i32) -> String {
    match check {
        FirewallCheck::Unknown(error) => format!(
            "{}{error}",
            i18n::tr(
                language,
                "防火墙：无法确认，",
                "Firewall: unknown, ",
                "ファイアウォール: 確認不可、",
            )
        ),
        FirewallCheck::AllDisabled => i18n::tr(
            language,
            "防火墙：全部关闭",
            "Firewall: all profiles disabled",
            "ファイアウォール: すべて無効",
        )
        .to_string(),
        FirewallCheck::Enabled(profiles) => format!(
            "{}{}{}",
            i18n::tr(
                language,
                "防火墙：已开启 ",
                "Firewall: enabled on ",
                "ファイアウォール: 有効 ",
            ),
            profiles.join("/"),
            i18n::tr(
                language,
                "，建议关闭或放行工具箱、游戏、node.exe、TCP 6969/7777/8000/9000、UDP 7777/9000",
                "; disable it or allow the toolbox, game, node.exe, TCP 6969/7777/8000/9000, and UDP 7777/9000",
                "。無効化するかツール、ゲーム、node.exe、TCP 6969/7777/8000/9000、UDP 7777/9000 を許可してください",
            )
        ),
    }
}
