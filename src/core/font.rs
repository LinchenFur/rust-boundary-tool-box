//! UI 字体检测、下载和当前用户安装。

use std::env;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::util::ensure_dir;
use super::*;
use anyhow::{Context, Result, bail};
use crc32fast::Hasher as Crc32Hasher;
use flate2::read::DeflateDecoder;
use serde::Deserialize;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{AddFontResourceExW, FONT_RESOURCE_CHARACTERISTICS};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_FONTCHANGE,
};
use windows::core::PCWSTR;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use winreg::{HKEY, RegKey};

const FONTS_REGISTRY_PATH: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts";

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

struct FontReleaseAsset {
    release_tag: String,
    zip_name: String,
    zip_url: String,
}

#[derive(Debug, Clone)]
struct RemoteZipEntry {
    name: String,
    compression_method: u16,
    crc32: u32,
    compressed_size: u64,
    uncompressed_size: u64,
    local_header_offset: u64,
}

const UI_FONT_SUFFIXES: &[&str] = &[
    "regular.ttf",
    "medium.ttf",
    "semibold.ttf",
    "bold.ttf",
    "extrabold.ttf",
];

impl InstallerCore {
    /// 检测系统或当前用户字体表中是否已经注册 UI 字体。
    pub fn ui_font_installed(&self) -> Result<bool> {
        Ok(font_registry_contains(HKEY_CURRENT_USER)?
            || font_registry_contains(HKEY_LOCAL_MACHINE)?)
    }

    /// 下载 Maple Mono CN 并安装到当前用户字体目录，同时报告可见进度。
    pub fn install_ui_font_with_progress(&self, progress: ProgressReporter) -> Result<String> {
        if self.ui_font_installed()? {
            report_font_progress(
                &progress,
                1.0,
                "字体已安装",
                &format!("{UI_FONT_FAMILY} 已安装，无需重复下载。"),
            );
            return Ok(format!("{UI_FONT_FAMILY} 已安装，无需重复下载。"));
        }

        report_font_progress(
            &progress,
            0.02,
            "准备字体",
            "未检测到界面字体，准备获取最新字体包。",
        );
        self.log(format!(
            "字体检查：未检测到 {UI_FONT_FAMILY}，准备自动下载。"
        ));
        report_font_progress(
            &progress,
            0.08,
            "查询字体版本",
            "正在读取 Maple Mono 最新 Release。",
        );
        let asset = fetch_maple_font_asset()?;
        self.log(format!(
            "下载字体：{} / {}",
            asset.release_tag, asset.zip_name
        ));

        let installed = self.cached_or_downloaded_ui_fonts(&asset, &progress)?;
        if installed == 0 {
            bail!("字体包中没有找到工具箱需要的 UI 字体文件。");
        }
        broadcast_font_change();
        report_font_progress(
            &progress,
            1.0,
            "字体安装完成",
            &format!("已安装/更新 {installed} 个字体文件。"),
        );
        self.log(format!(
            "字体安装完成：已写入 {} 个字体文件，来源 {}。",
            installed, asset.release_tag
        ));

        Ok(format!(
            "{UI_FONT_FAMILY} 自动安装完成。\n已安装/更新 {installed} 个字体文件。\n来源：{}\n\n如果当前界面没有立刻切换字体，请重启工具箱。",
            asset.release_tag
        ))
    }

    fn cached_or_downloaded_ui_fonts(
        &self,
        asset: &FontReleaseAsset,
        progress: &ProgressReporter,
    ) -> Result<usize> {
        let cache_dir = self.installer_home.join("fonts").join(&asset.release_tag);
        ensure_dir(&cache_dir)?;

        report_font_progress(
            progress,
            0.12,
            "测速下载代理",
            "正在测试 GitHub 代理节点速度。",
        );
        let selection = select_fastest_github_proxy(&self.github_proxy_prefix(), &asset.zip_url);
        self.log(format!(
            "字体下载代理：{}；可用 {}/{}",
            selection.display_label(),
            selection.reachable_count,
            selection.tested_count
        ));
        report_font_progress(
            progress,
            0.16,
            "下载字体",
            &format!(
                "使用下载代理：{}；可用 {}/{}",
                selection.display_label(),
                selection.reachable_count,
                selection.tested_count
            ),
        );

        let source_url = proxied_github_url(&selection.prefix, &asset.zip_url);
        report_font_progress(progress, 0.20, "读取字体目录", "正在读取远程字体包目录。");
        let client = http_client()?;
        let entries = fetch_remote_zip_entries(&client, &source_url)?;
        let selected = select_ui_font_entries(&entries)?;
        let mut installed = 0;
        for (index, entry) in selected.iter().enumerate() {
            let value = 0.24 + (index as f32 / selected.len() as f32) * 0.58;
            let file_name = font_file_name(&entry.name)
                .with_context(|| format!("字体条目名称无效：{}", entry.name))?;
            let cache_path = cache_dir.join(&file_name);
            let bytes = match cached_font_bytes(&cache_path, entry)? {
                Some(bytes) => {
                    report_font_progress(
                        progress,
                        value,
                        "校验字体缓存",
                        &format!("字体文件已缓存：{}", cache_path.display()),
                    );
                    bytes
                }
                None => {
                    report_font_progress(
                        progress,
                        value,
                        "下载字体",
                        &format!(
                            "下载字体文件 {}/{}：{}",
                            index + 1,
                            selected.len(),
                            file_name
                        ),
                    );
                    let bytes = download_remote_zip_entry(&client, &source_url, entry)?;
                    fs::write(&cache_path, &bytes)
                        .with_context(|| format!("写入字体缓存失败：{}", cache_path.display()))?;
                    bytes
                }
            };
            report_font_progress(
                progress,
                value + 0.03,
                "安装字体",
                &format!("正在安装字体文件：{file_name}"),
            );
            install_font_file_bytes(&file_name, &bytes)?;
            installed += 1;
        }
        Ok(installed)
    }
}

fn font_registry_contains(root: HKEY) -> Result<bool> {
    let root = RegKey::predef(root);
    let Ok(key) = root.open_subkey(FONTS_REGISTRY_PATH) else {
        return Ok(false);
    };
    let needle = UI_FONT_FAMILY.to_ascii_lowercase();
    for value in key.enum_values() {
        let (name, _) = value?;
        if name.to_ascii_lowercase().contains(&needle) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn fetch_maple_font_asset() -> Result<FontReleaseAsset> {
    let text = download_text(MAPLE_FONT_LATEST_RELEASE_API)?;
    let release = serde_json::from_str::<GitHubRelease>(&text)
        .context("解析 Maple Mono 最新 Release 响应失败")?;
    let zip = release
        .assets
        .iter()
        .find(|asset| asset.name.eq_ignore_ascii_case(MAPLE_FONT_RELEASE_ASSET))
        .or_else(|| {
            release
                .assets
                .iter()
                .find(|asset| asset.name.eq_ignore_ascii_case("MapleMono-CN.zip"))
        })
        .context("Maple Mono 最新 Release 中没有找到 CN 字体包")?;
    Ok(FontReleaseAsset {
        release_tag: release.tag_name,
        zip_name: zip.name.clone(),
        zip_url: zip.browser_download_url.clone(),
    })
}

fn http_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(240))
        .user_agent(format!("boundary-toolbox/{}", APP_VERSION))
        .build()
        .context("创建字体下载 HTTP 客户端失败")
}

fn download_text(url: &str) -> Result<String> {
    http_client()?
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .with_context(|| format!("请求失败：{url}"))?
        .error_for_status()
        .with_context(|| format!("服务器返回错误：{url}"))?
        .text()
        .with_context(|| format!("读取响应失败：{url}"))
}

fn report_font_progress(progress: &ProgressReporter, value: f32, title: &str, detail: &str) {
    progress(InstallProgress {
        value: value.clamp(0.0, 1.0),
        title: title.to_string(),
        detail: detail.to_string(),
    });
}

fn user_fonts_dir() -> Result<PathBuf> {
    let local_appdata = env::var("LOCALAPPDATA").context("未找到 LOCALAPPDATA")?;
    Ok(PathBuf::from(local_appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Fonts"))
}

fn fetch_remote_zip_entries(
    client: &reqwest::blocking::Client,
    url: &str,
) -> Result<Vec<RemoteZipEntry>> {
    let length = remote_content_length(client, url)?;
    let tail_size = length.min(128 * 1024);
    let tail_start = length - tail_size;
    let tail = download_range(client, url, tail_start, length - 1)?;
    let eocd_offset = find_eocd(&tail).context("远程字体包不是有效 ZIP：缺少中央目录")?;
    let eocd = &tail[eocd_offset..eocd_offset + 22];
    let entry_count = read_u16(eocd, 10) as usize;
    let central_size = read_u32(eocd, 12) as u64;
    let central_offset = read_u32(eocd, 16) as u64;
    if central_size == u32::MAX as u64 || central_offset == u32::MAX as u64 {
        bail!("远程字体包使用 ZIP64，当前不支持按需读取。");
    }
    let central = download_range(
        client,
        url,
        central_offset,
        central_offset + central_size.saturating_sub(1),
    )?;
    parse_central_directory(&central, entry_count)
}

fn remote_content_length(client: &reqwest::blocking::Client, url: &str) -> Result<u64> {
    if let Ok(response) = client
        .head(url)
        .send()
        .and_then(|response| response.error_for_status())
        && let Some(length) = response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
    {
        return Ok(length);
    }

    let response = client
        .get(url)
        .header(reqwest::header::RANGE, "bytes=0-0")
        .send()
        .with_context(|| format!("读取远程字体包大小失败：{url}"))?
        .error_for_status()
        .with_context(|| format!("远程字体包大小接口返回错误：{url}"))?;
    parse_content_range_total(response.headers())
        .or_else(|| response.content_length())
        .context("远程字体包响应中没有 Content-Length/Content-Range")
}

fn parse_content_range_total(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit('/').next())
        .and_then(|value| value.parse::<u64>().ok())
}

fn download_range(
    client: &reqwest::blocking::Client,
    url: &str,
    start: u64,
    end: u64,
) -> Result<Vec<u8>> {
    if end < start {
        return Ok(Vec::new());
    }
    let mut response = client
        .get(url)
        .header(reqwest::header::RANGE, format!("bytes={start}-{end}"))
        .send()
        .with_context(|| format!("请求远程字体片段失败：{url}"))?
        .error_for_status()
        .with_context(|| format!("远程字体片段接口返回错误：{url}"))?;
    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        bail!("下载代理不支持 Range 请求，拒绝下载完整字体包。");
    }
    let mut bytes = Vec::new();
    response
        .read_to_end(&mut bytes)
        .with_context(|| format!("读取远程字体片段失败：{url}"))?;
    Ok(bytes)
}

fn find_eocd(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .rposition(|window| window == [b'P', b'K', 0x05, 0x06])
}

fn parse_central_directory(bytes: &[u8], expected_count: usize) -> Result<Vec<RemoteZipEntry>> {
    let mut entries = Vec::with_capacity(expected_count);
    let mut offset = 0usize;
    while offset + 46 <= bytes.len() {
        if &bytes[offset..offset + 4] != b"PK\x01\x02" {
            bail!("远程字体包中央目录损坏。");
        }
        let compression_method = read_u16(bytes, offset + 10);
        let crc32 = read_u32(bytes, offset + 16);
        let compressed_size = read_u32(bytes, offset + 20) as u64;
        let uncompressed_size = read_u32(bytes, offset + 24) as u64;
        let name_len = read_u16(bytes, offset + 28) as usize;
        let extra_len = read_u16(bytes, offset + 30) as usize;
        let comment_len = read_u16(bytes, offset + 32) as usize;
        let local_header_offset = read_u32(bytes, offset + 42) as u64;
        let name_start = offset + 46;
        let name_end = name_start + name_len;
        if name_end > bytes.len() {
            bail!("远程字体包中央目录文件名越界。");
        }
        let name = String::from_utf8_lossy(&bytes[name_start..name_end]).to_string();
        entries.push(RemoteZipEntry {
            name,
            compression_method,
            crc32,
            compressed_size,
            uncompressed_size,
            local_header_offset,
        });
        offset = name_end + extra_len + comment_len;
    }
    if entries.len() != expected_count {
        bail!(
            "远程字体包中央目录条目数量异常：期望 {expected_count}，实际 {}。",
            entries.len()
        );
    }
    Ok(entries)
}

fn select_ui_font_entries(entries: &[RemoteZipEntry]) -> Result<Vec<RemoteZipEntry>> {
    let mut selected = Vec::new();
    for suffix in UI_FONT_SUFFIXES {
        let entry = entries
            .iter()
            .find(|entry| ui_font_file_matches(&entry.name, suffix))
            .with_context(|| format!("字体包中缺少 UI 所需字重：{suffix}"))?;
        selected.push(entry.clone());
    }
    Ok(selected)
}

fn ui_font_file_matches(entry_name: &str, suffix: &str) -> bool {
    let Some(file_name) = font_file_name(entry_name) else {
        return false;
    };
    let lower = file_name.to_ascii_lowercase();
    lower
        .strip_prefix("maplemono-cn-")
        .is_some_and(|weight| weight == suffix)
}

fn cached_font_bytes(path: &Path, entry: &RemoteZipEntry) -> Result<Option<Vec<u8>>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path).with_context(|| format!("读取字体缓存失败：{}", path.display()))?;
    if verify_font_bytes(&bytes, entry).is_ok() {
        Ok(Some(bytes))
    } else {
        Ok(None)
    }
}

fn download_remote_zip_entry(
    client: &reqwest::blocking::Client,
    url: &str,
    entry: &RemoteZipEntry,
) -> Result<Vec<u8>> {
    let header = download_range(
        client,
        url,
        entry.local_header_offset,
        entry.local_header_offset + 29,
    )?;
    if header.len() != 30 || &header[..4] != b"PK\x03\x04" {
        bail!("远程字体包本地文件头损坏：{}", entry.name);
    }
    let name_len = read_u16(&header, 26) as u64;
    let extra_len = read_u16(&header, 28) as u64;
    let data_start = entry.local_header_offset + 30 + name_len + extra_len;
    let compressed = download_range(
        client,
        url,
        data_start,
        data_start + entry.compressed_size.saturating_sub(1),
    )?;
    let bytes = match entry.compression_method {
        0 => compressed,
        8 => {
            let mut decoder = DeflateDecoder::new(Cursor::new(compressed));
            let mut decoded = Vec::new();
            decoder
                .read_to_end(&mut decoded)
                .with_context(|| format!("解压字体文件失败：{}", entry.name))?;
            decoded
        }
        method => bail!("字体文件使用不支持的 ZIP 压缩方式：{method}"),
    };
    verify_font_bytes(&bytes, entry)?;
    Ok(bytes)
}

fn verify_font_bytes(bytes: &[u8], entry: &RemoteZipEntry) -> Result<()> {
    if bytes.len() as u64 != entry.uncompressed_size {
        bail!(
            "字体文件大小校验失败：{}，期望 {}，实际 {}",
            entry.name,
            entry.uncompressed_size,
            bytes.len()
        );
    }
    let mut hasher = Crc32Hasher::new();
    hasher.update(bytes);
    let actual = hasher.finalize();
    if actual != entry.crc32 {
        bail!(
            "字体文件 CRC32 校验失败：{}，期望 {:08x}，实际 {:08x}",
            entry.name,
            entry.crc32,
            actual
        );
    }
    Ok(())
}

fn install_font_file_bytes(file_name: &str, bytes: &[u8]) -> Result<()> {
    let fonts_dir = user_fonts_dir()?;
    ensure_dir(&fonts_dir)?;
    let target_path = fonts_dir.join(file_name);
    fs::write(&target_path, bytes)
        .with_context(|| format!("写入字体文件失败：{}", target_path.display()))?;
    register_user_font(&target_path)?;
    load_font_resource(&target_path);
    Ok(())
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn font_file_name(entry_name: &str) -> Option<String> {
    let normalized = entry_name.replace('\\', "/");
    let file_name = normalized.rsplit('/').next()?.trim();
    if file_name.is_empty() {
        return None;
    }
    let lower = file_name.to_ascii_lowercase();
    if !lower.starts_with("maplemono") || !(lower.ends_with(".ttf") || lower.ends_with(".otf")) {
        return None;
    }
    Some(file_name.to_string())
}

fn register_user_font(path: &Path) -> Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(FONTS_REGISTRY_PATH)
        .context("打开当前用户字体注册表失败")?;
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .context("字体文件缺少文件名")?;
    key.set_value(font_registry_name(&file_name), &path.display().to_string())
        .context("写入当前用户字体注册表失败")
}

fn font_registry_name(file_name: &str) -> String {
    let path = Path::new(file_name);
    let stem = path
        .file_stem()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_name.to_string());
    let kind = match path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("otf") => "OpenType",
        _ => "TrueType",
    };
    let family = stem
        .strip_prefix("MapleMono")
        .map(|suffix| format!("Maple Mono{}", suffix))
        .unwrap_or(stem)
        .replace('-', " ");
    format!("{} ({kind})", family.trim())
}

fn load_font_resource(path: &Path) {
    let wide = path_to_wide(path);
    unsafe {
        let _ = AddFontResourceExW(
            PCWSTR(wide.as_ptr()),
            FONT_RESOURCE_CHARACTERISTICS(0),
            None,
        );
    }
}

fn broadcast_font_change() {
    unsafe {
        let _ = SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_FONTCHANGE,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            1000,
            None,
        );
    }
}

fn path_to_wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> RemoteZipEntry {
        RemoteZipEntry {
            name: name.to_string(),
            compression_method: 8,
            crc32: 0,
            compressed_size: 1,
            uncompressed_size: 1,
            local_header_offset: 0,
        }
    }

    #[test]
    fn selects_only_ui_font_weights() {
        let entries = vec![
            entry("MapleMono-CN-Regular.ttf"),
            entry("MapleMono-CN-Medium.ttf"),
            entry("MapleMono-CN-SemiBold.ttf"),
            entry("MapleMono-CN-Bold.ttf"),
            entry("MapleMono-CN-ExtraBold.ttf"),
            entry("MapleMono-CN-Italic.ttf"),
            entry("LICENSE.txt"),
        ];

        let selected = select_ui_font_entries(&entries).unwrap();
        let names = selected
            .into_iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            vec![
                "MapleMono-CN-Regular.ttf",
                "MapleMono-CN-Medium.ttf",
                "MapleMono-CN-SemiBold.ttf",
                "MapleMono-CN-Bold.ttf",
                "MapleMono-CN-ExtraBold.ttf",
            ]
        );
    }

    #[test]
    fn rejects_non_cn_or_italic_font_entries() {
        assert!(ui_font_file_matches(
            "MapleMono-CN-Regular.ttf",
            "regular.ttf"
        ));
        assert!(!ui_font_file_matches(
            "MapleMono-NF-CN-Regular.ttf",
            "regular.ttf"
        ));
        assert!(!ui_font_file_matches(
            "MapleMono-CN-Italic.ttf",
            "regular.ttf"
        ));
    }
}
