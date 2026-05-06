//! 应用级偏好设置读写，保存不属于游戏安装目录的 UI 状态。

use std::fs::{self, File};
use std::io::Write;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
#[cfg(windows)]
use windows::core::PCWSTR;

const PREFS_FILE_NAME: &str = "app_config.json";
const DEFAULT_VNT_SERVER: &str = "101.35.230.139:6660";

/// 用户在联机页填写的持久化设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct VntPrefs {
    #[serde(default)]
    pub server_text: String,
    #[serde(default)]
    pub server_options: Vec<String>,
    #[serde(default)]
    pub network_code: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub no_tun: bool,
    #[serde(default)]
    pub compress: bool,
    #[serde(default)]
    pub rtx: bool,
}

impl Default for VntPrefs {
    fn default() -> Self {
        Self {
            server_text: DEFAULT_VNT_SERVER.to_string(),
            server_options: vec![DEFAULT_VNT_SERVER.to_string()],
            network_code: String::new(),
            password: String::new(),
            no_tun: false,
            compress: false,
            rtx: false,
        }
    }
}

fn default_github_proxy_prefix() -> String {
    crate::core::DEFAULT_GITHUB_PROXY_PREFIX.to_string()
}

/// 整个工具箱的应用级偏好设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AppPrefs {
    #[serde(default)]
    pub language: i32,
    #[serde(default = "default_github_proxy_prefix")]
    pub github_proxy_prefix: String,
    #[serde(default)]
    pub vnt: VntPrefs,
}

impl Default for AppPrefs {
    fn default() -> Self {
        Self {
            language: 0,
            github_proxy_prefix: default_github_proxy_prefix(),
            vnt: VntPrefs::default(),
        }
    }
}

impl AppPrefs {
    /// 从 installer_tool/app_config.json 读取偏好设置；文件不存在时使用默认值。
    pub(crate) fn load(installer_home: &Path) -> Result<Self> {
        let path = prefs_path(installer_home);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("读取应用配置失败：{}", path.display()))?;
        let mut prefs: Self = serde_json::from_str(&text)
            .with_context(|| format!("解析应用配置失败：{}", path.display()))?;
        prefs.normalize();
        Ok(prefs)
    }

    /// 写入当前偏好设置。
    pub(crate) fn save(&self, installer_home: &Path) -> Result<()> {
        let path = prefs_path(installer_home);
        let text = serde_json::to_string_pretty(self)?;
        write_atomic(&path, text.as_bytes())
            .with_context(|| format!("写入应用配置失败：{}", path.display()))?;
        Ok(())
    }

    /// 配置损坏时先保留原文件副本，防止默认配置在退出时覆盖唯一线索。
    pub(crate) fn preserve_invalid(installer_home: &Path) -> Result<Option<PathBuf>> {
        let path = prefs_path(installer_home);
        if !path.exists() {
            return Ok(None);
        }
        let parent = path
            .parent()
            .with_context(|| format!("应用配置缺少父目录：{}", path.display()))?;
        fs::create_dir_all(parent)?;
        let backup = parent.join(format!(
            "app_config.invalid.{}.{}.json",
            chrono::Local::now().format("%Y%m%d_%H%M%S"),
            Uuid::new_v4().simple()
        ));
        fs::copy(&path, &backup).with_context(|| {
            format!(
                "备份损坏应用配置失败：{} -> {}",
                path.display(),
                backup.display()
            )
        })?;
        Ok(Some(backup))
    }

    /// 补齐默认 VNT 服务器，并去掉空白/重复项。
    fn normalize(&mut self) {
        self.language = self.language.clamp(0, 2);
        self.github_proxy_prefix =
            crate::core::normalize_github_proxy_prefix(&self.github_proxy_prefix);

        let mut normalized = Vec::new();
        for option in std::iter::once(self.vnt.server_text.as_str())
            .chain(self.vnt.server_options.iter().map(String::as_str))
            .chain(std::iter::once(DEFAULT_VNT_SERVER))
        {
            let option = option.trim();
            if option.is_empty() {
                continue;
            }
            if normalized
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(option))
            {
                continue;
            }
            normalized.push(option.to_string());
        }

        self.vnt.server_options = normalized;
        if self.vnt.server_text.trim().is_empty() {
            self.vnt.server_text = DEFAULT_VNT_SERVER.to_string();
        } else {
            self.vnt.server_text = self.vnt.server_text.trim().to_string();
        }
        self.vnt.network_code = self.vnt.network_code.trim().to_string();
    }
}

fn prefs_path(installer_home: &Path) -> PathBuf {
    installer_home.join(PREFS_FILE_NAME)
}

/// 写入同目录临时文件后替换目标文件，避免写到一半留下半截 JSON。
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("应用配置缺少父目录：{}", path.display()))?;
    fs::create_dir_all(parent)?;
    let temp_path = temp_file_for(path)?;
    {
        let mut output = File::create(&temp_path)
            .with_context(|| format!("创建临时配置失败：{}", temp_path.display()))?;
        output
            .write_all(bytes)
            .with_context(|| format!("写入临时配置失败：{}", temp_path.display()))?;
        output
            .sync_all()
            .with_context(|| format!("刷新临时配置失败：{}", temp_path.display()))?;
    }

    if let Err(error) = replace_file(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    Ok(())
}

/// 在目标同目录生成临时路径，确保替换动作发生在同一卷内。
fn temp_file_for(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .with_context(|| format!("应用配置缺少父目录：{}", path.display()))?;
    let file_name = path
        .file_name()
        .with_context(|| format!("应用配置缺少文件名：{}", path.display()))?
        .to_string_lossy();
    Ok(parent.join(format!(".{}.{}.tmp", file_name, Uuid::new_v4().simple())))
}

/// Windows 下使用 MoveFileExW 做原子替换。
#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> Result<()> {
    let source_wide = path_to_wide(source);
    let target_wide = path_to_wide(target);
    unsafe {
        MoveFileExW(
            PCWSTR(source_wide.as_ptr()),
            PCWSTR(target_wide.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .with_context(|| {
        format!(
            "替换应用配置失败：{} -> {}",
            source.display(),
            target.display()
        )
    })
}

/// 非 Windows 平台的原子替换。
#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> Result<()> {
    fs::rename(source, target).with_context(|| {
        format!(
            "替换应用配置失败：{} -> {}",
            source.display(),
            target.display()
        )
    })
}

/// 将 Rust 路径转换为以 0 结尾的 UTF-16 Windows 字符串。
#[cfg(windows)]
fn path_to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
