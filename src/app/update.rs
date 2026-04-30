//! GitHub Release 更新检查。

use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::core::APP_VERSION;

const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/LinchenFur/rust-boundary-tool-box/releases/latest";

/// GitHub 最新 release 的检查结果。
#[derive(Debug, Clone)]
pub(crate) struct UpdateCheckResult {
    pub(crate) latest_tag: String,
    pub(crate) latest_version: String,
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
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

/// 请求 GitHub latest release，并和当前版本比较。
pub(crate) fn check_latest_release() -> Result<UpdateCheckResult> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
        .context("创建更新检查 HTTP 客户端失败")?;
    let text = client
        .get(LATEST_RELEASE_API)
        .header("User-Agent", format!("boundary-toolbox/{APP_VERSION}"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .context("请求 GitHub 最新 Release 失败")?
        .error_for_status()
        .context("GitHub 最新 Release 接口返回错误")?
        .text()
        .context("读取 GitHub 最新 Release 响应失败")?;
    let release = serde_json::from_str::<GitHubRelease>(&text)
        .context("解析 GitHub 最新 Release 响应失败")?;
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
        latest_version,
    })
}

/// 设置页紧凑状态文本。
pub(crate) fn update_status_text(result: &UpdateCheckResult) -> String {
    if result.is_newer {
        format!("发现新版本：v{}", result.latest_version)
    } else {
        format!("已是最新：v{APP_VERSION}")
    }
}

/// 弹窗详细文本。
pub(crate) fn update_dialog_text(result: &UpdateCheckResult) -> String {
    let download = result
        .asset_url
        .as_deref()
        .unwrap_or("该 Release 未提供 boundary_toolbox.exe 资产");
    if result.is_newer {
        format!(
            "发现新版本：{}\n当前版本：v{}\n发布时间：{}\n\n{}\n\n下载地址：{}",
            result.latest_tag, APP_VERSION, result.published_at, result.release_name, download
        )
    } else {
        format!(
            "当前已经是最新版本。\n当前版本：v{}\n最新 Release：{}\n发布时间：{}\n\n{}",
            APP_VERSION, result.latest_tag, result.published_at, result.release_url
        )
    }
}

fn normalize_version(value: &str) -> String {
    value
        .trim()
        .trim_start_matches(['v', 'V'])
        .trim()
        .to_string()
}

fn is_version_newer(latest: &str, current: &str) -> bool {
    let latest_parts = version_parts(latest);
    let current_parts = version_parts(current);
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
    normalize_version(value)
        .split(['.', '-', '+'])
        .map(|part| {
            part.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse::<u64>()
                .unwrap_or(0)
        })
        .collect()
}
