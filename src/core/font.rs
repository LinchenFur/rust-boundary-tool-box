//! UI 字体检测、下载和当前用户安装。

use std::env;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{AddFontResourceExW, FONT_RESOURCE_CHARACTERISTICS};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_FONTCHANGE,
};
use windows::core::PCWSTR;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use winreg::{HKEY, RegKey};
use zip::ZipArchive;

use super::util::ensure_dir;
use super::*;

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
    sha_url: Option<String>,
}

impl InstallerCore {
    /// 检测系统或当前用户字体表中是否已经注册 UI 字体。
    pub fn ui_font_installed(&self) -> Result<bool> {
        Ok(font_registry_contains(HKEY_CURRENT_USER)?
            || font_registry_contains(HKEY_LOCAL_MACHINE)?)
    }

    /// 下载 Maple Mono NF CN 并安装到当前用户字体目录。
    pub fn install_ui_font(&self) -> Result<String> {
        if self.ui_font_installed()? {
            return Ok(format!("{UI_FONT_FAMILY} 已安装，无需重复下载。"));
        }

        self.log(format!(
            "字体检查：未检测到 {UI_FONT_FAMILY}，准备自动下载。"
        ));
        let asset = fetch_maple_font_asset()?;
        self.log(format!(
            "下载字体：{} / {}",
            asset.release_tag, asset.zip_name
        ));

        let zip_bytes = self.cached_or_downloaded_font_zip(&asset)?;
        let installed = install_font_archive(&zip_bytes)?;
        if installed == 0 {
            bail!("字体压缩包中没有找到可安装的 TTF/OTF 文件。");
        }
        broadcast_font_change();
        self.log(format!(
            "字体安装完成：已写入 {} 个字体文件，来源 {}。",
            installed, asset.release_tag
        ));

        Ok(format!(
            "{UI_FONT_FAMILY} 自动安装完成。\n已安装/更新 {installed} 个字体文件。\n来源：{}\n\n如果当前界面没有立刻切换字体，请重启工具箱。",
            asset.release_tag
        ))
    }

    fn cached_or_downloaded_font_zip(&self, asset: &FontReleaseAsset) -> Result<Vec<u8>> {
        let cache_dir = self.installer_home.join("fonts");
        ensure_dir(&cache_dir)?;
        let zip_path = cache_dir.join(&asset.zip_name);
        let expected_sha = match &asset.sha_url {
            Some(url) => download_text(&self.proxied_github_url(url))
                .ok()
                .and_then(|text| parse_sha256(&text)),
            None => None,
        };

        if zip_path.exists() {
            let bytes = fs::read(&zip_path)
                .with_context(|| format!("读取字体缓存失败：{}", zip_path.display()))?;
            if expected_sha
                .as_deref()
                .is_none_or(|expected| sha256_hex(&bytes).eq_ignore_ascii_case(expected))
            {
                self.log(format!("使用字体缓存：{}", zip_path.display()));
                return Ok(bytes);
            }
            self.log(format!(
                "字体缓存校验失败，重新下载：{}",
                zip_path.display()
            ));
        }

        let bytes = download_bytes(&self.proxied_github_url(&asset.zip_url))?;
        if let Some(expected) = expected_sha {
            let actual = sha256_hex(&bytes);
            if !actual.eq_ignore_ascii_case(&expected) {
                bail!("字体包校验失败：期望 {expected}，实际 {actual}");
            }
        }
        fs::write(&zip_path, &bytes)
            .with_context(|| format!("写入字体缓存失败：{}", zip_path.display()))?;
        Ok(bytes)
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
                .find(|asset| asset.name.eq_ignore_ascii_case("MapleMono-NF-CN.zip"))
        })
        .context("Maple Mono 最新 Release 中没有找到 NF-CN 字体包")?;
    let sha_name = zip.name.replace(".zip", ".sha256");
    let sha_url = release
        .assets
        .iter()
        .find(|asset| asset.name.eq_ignore_ascii_case(&sha_name))
        .map(|asset| asset.browser_download_url.clone());

    Ok(FontReleaseAsset {
        release_tag: release.tag_name,
        zip_name: zip.name.clone(),
        zip_url: zip.browser_download_url.clone(),
        sha_url,
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

fn download_bytes(url: &str) -> Result<Vec<u8>> {
    let bytes = http_client()?
        .get(url)
        .send()
        .with_context(|| format!("下载失败：{url}"))?
        .error_for_status()
        .with_context(|| format!("服务器返回错误：{url}"))?
        .bytes()
        .with_context(|| format!("读取下载内容失败：{url}"))?;
    Ok(bytes.to_vec())
}

fn parse_sha256(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|part| part.len() == 64 && part.chars().all(|ch| ch.is_ascii_hexdigit()))
        .map(ToOwned::to_owned)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn user_fonts_dir() -> Result<PathBuf> {
    let local_appdata = env::var("LOCALAPPDATA").context("未找到 LOCALAPPDATA")?;
    Ok(PathBuf::from(local_appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Fonts"))
}

fn install_font_archive(zip_bytes: &[u8]) -> Result<usize> {
    let fonts_dir = user_fonts_dir()?;
    ensure_dir(&fonts_dir)?;
    let mut archive = ZipArchive::new(Cursor::new(zip_bytes)).context("无法读取字体压缩包")?;
    let mut installed = 0;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        let Some(file_name) = font_file_name(entry.name()) else {
            continue;
        };
        let target_path = fonts_dir.join(&file_name);
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .with_context(|| format!("读取字体文件失败：{file_name}"))?;
        fs::write(&target_path, &bytes)
            .with_context(|| format!("写入字体文件失败：{}", target_path.display()))?;
        register_user_font(&target_path)?;
        load_font_resource(&target_path);
        installed += 1;
    }
    Ok(installed)
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
