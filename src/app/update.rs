//! GitHub Release 更新检查。

use std::time::Duration;

use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;

use crate::core::APP_VERSION;

const RELEASES_API: &str =
    "https://api.github.com/repos/LinchenFur/rust-boundary-tool-box/releases";

/// GitHub 最新 release 的检查结果。
#[derive(Debug, Clone)]
pub(crate) struct UpdateCheckResult {
    pub(crate) latest_tag: String,
    pub(crate) release_name: String,
    pub(crate) release_url: String,
    pub(crate) asset_url: Option<String>,
    pub(crate) published_at: String,
    pub(crate) is_newer: bool,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    name: Option<String>,
    html_url: String,
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

/// 请求 GitHub 最新发布的 Release，并和当前版本比较。
pub(crate) fn check_latest_release() -> Result<UpdateCheckResult> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
        .context("创建更新检查 HTTP 客户端失败")?;
    let text = client
        .get(RELEASES_API)
        .header("User-Agent", format!("boundary-toolbox/{APP_VERSION}"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .context("请求 GitHub Release 列表失败")?
        .error_for_status()
        .context("GitHub Release 列表接口返回错误")?
        .text()
        .context("读取 GitHub Release 列表响应失败")?;
    let releases = serde_json::from_str::<Vec<GitHubRelease>>(&text)
        .context("解析 GitHub Release 列表失败")?;
    let release = select_latest_visible_release(releases).context("GitHub 没有可用 Release")?;
    let latest_version = normalize_version(&release.tag_name);
    let asset_url = release
        .assets
        .iter()
        .find(|asset| asset.name.eq_ignore_ascii_case("boundary_toolbox.exe"))
        .or_else(|| {
            release
                .assets
                .iter()
                .find(|asset| asset.name.ends_with(".exe"))
        })
        .map(|asset| asset.browser_download_url.clone());

    Ok(UpdateCheckResult {
        latest_tag: release.tag_name.clone(),
        release_name: release.name.unwrap_or_else(|| release.tag_name.clone()),
        release_url: release.html_url,
        asset_url,
        published_at: release.published_at.unwrap_or_else(|| "-".to_string()),
        is_newer: is_version_newer(&latest_version, APP_VERSION),
    })
}

/// 设置页紧凑状态文本。
pub(crate) fn update_status_text(result: &UpdateCheckResult, language: i32) -> String {
    if result.is_newer {
        format!(
            "{}{}",
            crate::app::i18n::tr(language, "发现新版本：", "New version: ", "新バージョン: "),
            format_version_for_display(&result.latest_tag)
        )
    } else {
        format!(
            "{}{}",
            crate::app::i18n::tr(language, "已是最新：", "Up to date: ", "最新版です: "),
            format_version_for_display(APP_VERSION)
        )
    }
}

/// 弹窗详细文本。
pub(crate) fn update_dialog_text(result: &UpdateCheckResult, language: i32) -> String {
    let download = result.asset_url.as_deref().unwrap_or(crate::app::i18n::tr(
        language,
        "该 Release 未提供 boundary_toolbox.exe 资产",
        "This release does not include a boundary_toolbox.exe asset",
        "この Release には boundary_toolbox.exe が含まれていません",
    ));
    if result.is_newer {
        format!(
            "{}{}\n{}{}\n{}{}\n\n{}\n\n{}{}",
            crate::app::i18n::tr(language, "发现新版本：", "New version: ", "新バージョン: "),
            format_version_for_display(&result.latest_tag),
            crate::app::i18n::tr(
                language,
                "当前版本：",
                "Current version: ",
                "現在のバージョン: "
            ),
            format_version_for_display(APP_VERSION),
            crate::app::i18n::tr(language, "发布时间：", "Published: ", "公開日時: "),
            result.published_at,
            result.release_name,
            crate::app::i18n::tr(language, "下载地址：", "Download: ", "ダウンロード: "),
            download
        )
    } else {
        format!(
            "{}\n{}{}\n{}{}\n{}{}\n\n{}",
            crate::app::i18n::tr(
                language,
                "当前已经是最新版本。",
                "You are already on the latest version.",
                "現在のバージョンは最新版です。",
            ),
            crate::app::i18n::tr(
                language,
                "当前版本：",
                "Current version: ",
                "現在のバージョン: "
            ),
            format_version_for_display(APP_VERSION),
            crate::app::i18n::tr(
                language,
                "最新 Release：",
                "Latest release: ",
                "最新 Release: "
            ),
            format_version_for_display(&result.latest_tag),
            crate::app::i18n::tr(language, "发布时间：", "Published: ", "公開日時: "),
            result.published_at,
            result.release_url
        )
    }
}

fn format_version_for_display(value: &str) -> String {
    value
        .trim()
        .trim_start_matches(['v', 'V'])
        .trim()
        .to_string()
}

fn normalize_version(value: &str) -> String {
    value
        .trim()
        .trim_start_matches(['v', 'V'])
        .trim()
        .to_string()
}

fn select_latest_visible_release(releases: Vec<GitHubRelease>) -> Option<GitHubRelease> {
    releases.into_iter().find(|release| !release.draft)
}

fn is_version_newer(latest: &str, current: &str) -> bool {
    let latest_parts = version_parts(latest);
    let current_parts = version_parts(current);
    if latest_parts.is_empty() {
        return false;
    }
    let max_len = latest_parts.len().max(current_parts.len());
    for index in 0..max_len {
        let latest_value = *latest_parts.get(index).unwrap_or(&0);
        let current_value = *current_parts.get(index).unwrap_or(&0);
        if latest_value != current_value {
            return latest_value > current_value;
        }
    }
    false
}

fn version_parts(value: &str) -> Vec<u64> {
    let Ok(regex) = Regex::new(r"\d+") else {
        return Vec::new();
    };
    regex
        .find_iter(&normalize_version(value))
        .filter_map(|part| part.as_str().parse::<u64>().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_plain_and_v_prefixed_versions() {
        assert!(is_version_newer("v19.19.84", "19.19.83"));
        assert!(!is_version_newer("v19.19.84", "19.19.84"));
    }

    #[test]
    fn compares_beta_prefixed_versions() {
        assert!(is_version_newer("beta19.19.85", "19.19.84"));
        assert!(!is_version_newer("beta19.19.83", "19.19.84"));
    }

    #[test]
    fn extracts_numeric_parts_from_mixed_tags() {
        assert_eq!(
            version_parts("release-v19.19.84-hotfix.2"),
            vec![19, 19, 84, 2]
        );
    }

    #[test]
    fn selects_first_non_draft_release() {
        let releases = vec![
            GitHubRelease {
                tag_name: "v99.0.0".to_string(),
                name: None,
                html_url: "https://example.invalid/draft".to_string(),
                published_at: None,
                draft: true,
                assets: Vec::new(),
            },
            GitHubRelease {
                tag_name: "beta19.19.85".to_string(),
                name: None,
                html_url: "https://example.invalid/release".to_string(),
                published_at: None,
                draft: false,
                assets: Vec::new(),
            },
        ];

        let release = select_latest_visible_release(releases).expect("visible release");
        assert_eq!(release.tag_name, "beta19.19.85");
    }
}
