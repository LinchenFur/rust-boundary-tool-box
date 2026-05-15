//! GitHub Release 更新检查和下载。

use std::fs::{self, File};
use std::io::{Read, Write};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Deserialize;

use crate::core::{APP_VERSION, CREATE_NO_WINDOW, InstallProgress, ProgressReporter};

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

/// 下载最新 release 资产到运行目录；运行目录不可写时落到 installer_tool/downloads。
pub(crate) fn download_release_asset(
    result: &UpdateCheckResult,
    runtime_dir: &Path,
    fallback_dir: &Path,
    proxy_prefix: &str,
    progress: ProgressReporter,
) -> Result<PathBuf> {
    let Some(asset_url) = result.asset_url.as_deref() else {
        bail!("该 Release 未提供 boundary_toolbox.exe 资产");
    };
    let file_name = release_asset_file_name(&result.latest_tag);
    report_update_progress(
        &progress,
        0.02,
        "准备下载更新",
        "正在准备从 GitHub Release 下载更新。",
    );
    report_update_progress(
        &progress,
        0.04,
        "测速下载代理",
        "正在测试 GitHub 代理节点速度。",
    );
    let selection = crate::core::select_fastest_github_proxy(proxy_prefix, asset_url);
    let download_url = crate::core::proxied_github_url(&selection.prefix, asset_url);
    report_update_progress(
        &progress,
        0.05,
        "下载更新",
        format!(
            "使用下载代理：{}；可用 {}/{}",
            selection.display_label(),
            selection.reachable_count,
            selection.tested_count
        ),
    );
    match download_asset_to_dir(&download_url, runtime_dir, &file_name, &progress) {
        Ok(path) => Ok(path),
        Err(primary_error) => {
            report_update_progress(
                &progress,
                0.06,
                "下载更新",
                format!("运行目录不可写，改存到下载缓存：{}", fallback_dir.display()),
            );
            download_asset_to_dir(&download_url, fallback_dir, &file_name, &progress)
                .with_context(|| format!("下载到运行目录失败：{primary_error}"))
        }
    }
}

/// 生成并启动外部更新助手，等待当前进程退出后替换当前 exe 并重启。
pub(crate) fn schedule_self_replace_and_restart(
    downloaded_path: &Path,
    script_dir: &Path,
    progress: ProgressReporter,
) -> Result<()> {
    report_update_progress(&progress, 0.985, "准备替换更新", "正在生成自动替换脚本。");
    validate_downloaded_exe(downloaded_path)?;

    let current_exe = std::env::current_exe().context("获取当前程序路径失败")?;
    let current_dir = current_exe
        .parent()
        .map(Path::to_path_buf)
        .context("当前程序路径没有父目录")?;
    let exe_name = current_exe
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("boundary_toolbox.exe");
    fs::create_dir_all(script_dir)
        .with_context(|| format!("创建更新脚本目录失败：{}", script_dir.display()))?;
    let backup_path = current_exe.with_file_name(format!("{exe_name}.old"));
    let log_path = script_dir.join("self_update.log");
    let script_path = script_dir.join(format!("apply_update_{}.ps1", std::process::id()));
    let script = build_self_update_script(
        std::process::id(),
        downloaded_path,
        &current_exe,
        &backup_path,
        &current_dir,
        &log_path,
    );
    fs::write(&script_path, script)
        .with_context(|| format!("写入更新脚本失败：{}", script_path.display()))?;

    let mut command = Command::new("powershell");
    command
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-WindowStyle")
        .arg("Hidden")
        .arg("-File")
        .arg(&script_path);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
        .spawn()
        .with_context(|| format!("启动更新助手失败：{}", script_path.display()))?;
    report_update_progress(
        &progress,
        1.0,
        "准备重启",
        "工具箱将关闭，更新助手会替换当前文件并重新启动。",
    );
    Ok(())
}

fn build_self_update_script(
    process_id: u32,
    source: &Path,
    target: &Path,
    backup: &Path,
    working_dir: &Path,
    log_path: &Path,
) -> String {
    format!(
        r#"$ErrorActionPreference = 'Stop'
$ProcessIdToWait = {process_id}
$Source = {source}
$Target = {target}
$Backup = {backup}
$WorkingDir = {working_dir}
$LogPath = {log_path}
function Write-UpdateLog([string]$Text) {{
    $line = '[{{0}}] {{1}}' -f (Get-Date -Format o), $Text
    Add-Content -LiteralPath $LogPath -Encoding UTF8 -Value $line
}}
try {{
    Write-UpdateLog 'waiting for current process to exit'
    for ($i = 0; $i -lt 160; $i++) {{
        $running = Get-Process -Id $ProcessIdToWait -ErrorAction SilentlyContinue
        if ($null -eq $running) {{ break }}
        Start-Sleep -Milliseconds 250
    }}
    if ($null -ne (Get-Process -Id $ProcessIdToWait -ErrorAction SilentlyContinue)) {{
        throw 'current process did not exit in time'
    }}
    for ($try = 0; $try -lt 80; $try++) {{
        try {{
            if (!(Test-Path -LiteralPath $Source -PathType Leaf)) {{
                throw ('update source missing: ' + $Source)
            }}
            if (Test-Path -LiteralPath $Backup) {{
                Remove-Item -LiteralPath $Backup -Force -ErrorAction SilentlyContinue
            }}
            if (Test-Path -LiteralPath $Target) {{
                Move-Item -LiteralPath $Target -Destination $Backup -Force
            }}
            Move-Item -LiteralPath $Source -Destination $Target -Force
            Write-UpdateLog 'replacement complete; restarting'
            Start-Process -FilePath $Target -WorkingDirectory $WorkingDir
            if (Test-Path -LiteralPath $Backup) {{
                Remove-Item -LiteralPath $Backup -Force -ErrorAction SilentlyContinue
            }}
            Remove-Item -LiteralPath $MyInvocation.MyCommand.Path -Force -ErrorAction SilentlyContinue
            exit 0
        }} catch {{
            Write-UpdateLog ('replace attempt ' + $try + ' failed: ' + $_.Exception.Message)
            if ((Test-Path -LiteralPath $Backup -PathType Leaf) -and !(Test-Path -LiteralPath $Target)) {{
                Move-Item -LiteralPath $Backup -Destination $Target -Force -ErrorAction SilentlyContinue
            }}
            Start-Sleep -Milliseconds 250
        }}
    }}
    throw 'could not replace executable'
}} catch {{
    Write-UpdateLog ('fatal: ' + $_.Exception.ToString())
    exit 1
}}
"#,
        process_id = process_id,
        source = powershell_single_quoted_path(source),
        target = powershell_single_quoted_path(target),
        backup = powershell_single_quoted_path(backup),
        working_dir = powershell_single_quoted_path(working_dir),
        log_path = powershell_single_quoted_path(log_path),
    )
}

fn powershell_single_quoted_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\'', "''");
    format!("'{text}'")
}

fn release_asset_file_name(tag: &str) -> String {
    let version = format_version_for_display(tag);
    let version = version
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("boundary_toolbox-{version}.exe")
}

fn download_asset_to_dir(
    url: &str,
    dir: &Path,
    file_name: &str,
    progress: &ProgressReporter,
) -> Result<PathBuf> {
    fs::create_dir_all(dir).with_context(|| format!("创建下载目录失败：{}", dir.display()))?;
    let target = dir.join(file_name);
    let temp = dir.join(format!(".{file_name}.download"));
    if temp.exists() {
        fs::remove_file(&temp)
            .with_context(|| format!("删除旧临时文件失败：{}", temp.display()))?;
    }

    let mut response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .context("创建更新下载 HTTP 客户端失败")?
        .get(url)
        .header("User-Agent", format!("boundary-toolbox/{APP_VERSION}"))
        .send()
        .context("请求更新文件失败")?
        .error_for_status()
        .context("更新文件下载接口返回错误")?;
    let total_size = response.content_length();

    {
        let mut output = File::create(&temp)
            .with_context(|| format!("创建临时下载文件失败：{}", temp.display()))?;
        let mut buffer = [0u8; 64 * 1024];
        let mut downloaded = 0u64;
        let mut last_report = 0u64;
        loop {
            let count = response.read(&mut buffer).context("读取更新文件失败")?;
            if count == 0 {
                break;
            }
            output
                .write_all(&buffer[..count])
                .context("写入更新文件失败")?;
            downloaded += count as u64;
            if downloaded == total_size.unwrap_or_default()
                || downloaded.saturating_sub(last_report) >= 512 * 1024
            {
                last_report = downloaded;
                report_update_progress(
                    progress,
                    download_progress_value(downloaded, total_size),
                    "下载更新",
                    download_detail(downloaded, total_size),
                );
            }
        }
        output
            .flush()
            .with_context(|| format!("刷新临时下载文件失败：{}", temp.display()))?;
    }

    report_update_progress(
        progress,
        0.88,
        "校验更新文件",
        "正在校验 Windows 可执行文件。",
    );
    validate_downloaded_exe(&temp)?;
    report_update_progress(
        progress,
        0.94,
        "保存更新文件",
        format!("保存更新文件：{}", target.display()),
    );
    if target.exists() {
        fs::remove_file(&target)
            .with_context(|| format!("替换旧更新文件失败：{}", target.display()))?;
    }
    fs::rename(&temp, &target).with_context(|| {
        format!(
            "保存更新文件失败：{} -> {}",
            temp.display(),
            target.display()
        )
    })?;
    report_update_progress(
        progress,
        0.98,
        "保存更新文件",
        format!("保存更新文件：{}", target.display()),
    );
    Ok(target)
}

fn download_progress_value(downloaded: u64, total_size: Option<u64>) -> f32 {
    let fraction = match total_size {
        Some(total) if total > 0 => downloaded as f32 / total as f32,
        _ => downloaded as f32 / (80 * 1024 * 1024) as f32,
    };
    (0.08 + fraction.clamp(0.0, 1.0) * 0.78).min(0.86)
}

fn download_detail(downloaded: u64, total_size: Option<u64>) -> String {
    match total_size {
        Some(total) if total > 0 => format!(
            "更新文件：已下载 {} / {}",
            format_size(downloaded),
            format_size(total)
        ),
        _ => format!("更新文件：已下载 {}", format_size(downloaded)),
    }
}

fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let bytes = bytes as f64;
    if bytes >= MB {
        format!("{:.1} MB", bytes / MB)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes / KB)
    } else {
        format!("{bytes:.0} B")
    }
}

fn report_update_progress(
    progress: &ProgressReporter,
    value: f32,
    title: &str,
    detail: impl Into<String>,
) {
    progress(InstallProgress {
        value,
        title: title.to_string(),
        detail: detail.into(),
    });
}

fn validate_downloaded_exe(path: &Path) -> Result<()> {
    let mut file =
        File::open(path).with_context(|| format!("读取更新文件失败：{}", path.display()))?;
    let mut magic = [0u8; 2];
    file.read_exact(&mut magic)
        .with_context(|| format!("更新文件为空或不完整：{}", path.display()))?;
    if magic != *b"MZ" {
        bail!("下载结果不是有效的 Windows 可执行文件");
    }
    Ok(())
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
            "{}{}\n{}{}\n{}{}\n\n{}\n\n{}\n\n{}{}",
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
            crate::app::i18n::tr(
                language,
                "点击立即更新后会自动下载、替换当前程序并重启。",
                "Click Update Now to download, replace this executable, and restart automatically.",
                "今すぐ更新をクリックすると、自動でダウンロード、置き換え、再起動します。",
            ),
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

    #[test]
    fn creates_versioned_asset_file_name() {
        assert_eq!(
            release_asset_file_name("v19.19.91"),
            "boundary_toolbox-19.19.91.exe"
        );
        assert_eq!(
            release_asset_file_name("release v19/19/91"),
            "boundary_toolbox-release_v19_19_91.exe"
        );
    }

    #[test]
    fn escapes_powershell_single_quoted_paths() {
        let escaped = powershell_single_quoted_path(Path::new(r"C:\O'Brien\tool.exe"));
        assert_eq!(escaped, r"'C:\O''Brien\tool.exe'");
    }
}
