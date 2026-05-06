//! 内嵌载荷、在线 ProjectRebound 更新和安装记录统计。

use super::*;

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Cursor, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use walkdir::WalkDir;
use windows::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
use windows::core::PCWSTR;
use zip::ZipArchive;

use super::filesystem::delete_path;
use super::util::ensure_dir;

#[derive(Debug, Deserialize)]
struct NodeRelease {
    version: String,
    lts: serde_json::Value,
    #[serde(default)]
    files: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct NodeRuntimePackage {
    pub(crate) source_url: String,
    pub(crate) version: String,
    pub(crate) zip_name: String,
    pub(crate) cache_hit: bool,
    bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub(crate) struct BoundaryMetaServerPackage {
    pub(crate) source_url: String,
    pub(crate) zip_name: String,
    pub(crate) cache_hit: bool,
    bytes: Vec<u8>,
}

pub(crate) type DownloadProgress<'a> = &'a dyn Fn(u64, Option<u64>);

#[derive(Debug, Clone)]
pub(crate) struct ItemStats {
    pub(crate) size: u64,
    pub(crate) sha256: Option<String>,
    pub(crate) file_count: Option<u64>,
    pub(crate) dir_count: Option<u64>,
}

/// 打开构建期内嵌的载荷压缩包。
pub(crate) fn open_payload_archive() -> Result<ZipArchive<Cursor<&'static [u8]>>> {
    ZipArchive::new(Cursor::new(PAYLOAD_ZIP_BYTES)).context("无法读取内嵌载荷")
}

/// 将一个受管内嵌文件或目录解压到目标根目录。
pub(crate) fn extract_managed_item(item: &ManagedItem, target_root: &Path) -> Result<()> {
    let mut archive = open_payload_archive()?;
    match item.kind {
        ItemKind::File => {
            let mut entry = archive
                .by_name(item.name)
                .with_context(|| format!("内嵌载荷缺少文件 {}", item.name))?;
            let target = target_root.join(item.name);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut output = File::create(&target)?;
            io::copy(&mut entry, &mut output)?;
        }
        ItemKind::Dir => {
            let prefix = format!("{}/", item.name);
            let names: Vec<String> = archive
                .file_names()
                .filter(|name| *name == item.name || name.starts_with(&prefix))
                .map(ToOwned::to_owned)
                .collect();
            if names.is_empty() {
                bail!("内嵌载荷缺少目录 {}", item.name);
            }
            for name in names {
                let mut entry = archive.by_name(&name)?;
                let out_path = target_root.join(Path::new(&name));
                if entry.is_dir() {
                    fs::create_dir_all(&out_path)?;
                    continue;
                }
                if let Some(parent) = out_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut output = File::create(&out_path)?;
                io::copy(&mut entry, &mut output)?;
            }
        }
    }
    Ok(())
}

/// 判断文件是否来自在线 ProjectRebound release。
pub(crate) fn is_project_rebound_online_file(name: &str) -> bool {
    PROJECT_REBOUND_ONLINE_FILES
        .iter()
        .any(|item| item.eq_ignore_ascii_case(name))
}

/// Node.js 运行时不再打进 payload，而是在安装时按需下载。
pub(crate) fn is_nodejs_online_item(name: &str) -> bool {
    name.eq_ignore_ascii_case(NODEJS_DIR_NAME)
}

/// 登录服务器目录不再打进 payload，而是在安装时按需从 GitHub 下载。
pub(crate) fn is_boundary_meta_server_online_item(name: &str) -> bool {
    name.eq_ignore_ascii_case(BOUNDARY_META_SERVER_DIR_NAME)
}

/// 下载当前 ProjectRebound Nightly zip。
pub(crate) fn download_project_rebound_release(
    source_url: &str,
    progress: Option<DownloadProgress<'_>>,
    cancel: Option<&InstallCancelToken>,
) -> Result<Vec<u8>> {
    download_bytes_with_progress(source_url, Duration::from_secs(90), progress, cancel)
        .context("下载 ProjectRebound Nightly Release 失败")
}

/// 下载 BoundaryMetaServer main 分支源码包。
pub(crate) fn download_boundary_meta_server(
    cache_dir: &Path,
    source_url: &str,
    progress: Option<DownloadProgress<'_>>,
    cancel: Option<&InstallCancelToken>,
) -> Result<BoundaryMetaServerPackage> {
    check_download_cancel(cancel)?;
    ensure_dir(cache_dir)?;
    let zip_name = "BoundaryMetaServer-main.zip".to_string();
    let zip_path = cache_dir.join(&zip_name);
    if zip_path.exists() {
        let bytes = fs::read(&zip_path)
            .with_context(|| format!("读取 BoundaryMetaServer 缓存失败：{}", zip_path.display()))?;
        if boundary_meta_server_archive_valid(&bytes) {
            if let Some(progress) = progress {
                progress(bytes.len() as u64, Some(bytes.len() as u64));
            }
            return Ok(BoundaryMetaServerPackage {
                source_url: source_url.to_string(),
                zip_name,
                cache_hit: true,
                bytes,
            });
        }
    }

    let bytes =
        download_bytes_with_progress(source_url, Duration::from_secs(180), progress, cancel)
            .context("下载 BoundaryMetaServer 失败")?;
    check_download_cancel(cancel)?;
    if !boundary_meta_server_archive_valid(&bytes) {
        bail!("BoundaryMetaServer 压缩包结构无效：缺少 index.js。");
    }
    fs::write(&zip_path, &bytes)
        .with_context(|| format!("写入 BoundaryMetaServer 缓存失败：{}", zip_path.display()))?;
    Ok(BoundaryMetaServerPackage {
        source_url: source_url.to_string(),
        zip_name,
        cache_hit: false,
        bytes,
    })
}

/// 下载最新 LTS Node.js Windows zip，并用官方 SHASUMS256 校验。
pub(crate) fn download_node_runtime(
    cache_dir: &Path,
    progress: Option<DownloadProgress<'_>>,
    cancel: Option<&InstallCancelToken>,
) -> Result<NodeRuntimePackage> {
    check_download_cancel(cancel)?;
    let release = latest_node_lts_release()?;
    check_download_cancel(cancel)?;
    let arch = node_windows_arch()?;
    let file_key = format!("win-{arch}-zip");
    if !release.files.iter().any(|file| file == &file_key) {
        bail!("Node.js {} 不提供 Windows {} zip。", release.version, arch);
    }
    let zip_name = format!("node-{}-win-{}.zip", release.version, arch);
    let base_url = format!("https://nodejs.org/dist/{}/", release.version);
    let zip_url = format!("{base_url}{zip_name}");
    let shasums_url = format!("{base_url}SHASUMS256.txt");
    let expected_sha = download_text(&shasums_url)
        .ok()
        .and_then(|text| parse_shasum_for_file(&text, &zip_name));
    check_download_cancel(cancel)?;
    ensure_dir(cache_dir)?;
    let zip_path = cache_dir.join(&zip_name);

    if zip_path.exists() {
        let bytes = fs::read(&zip_path)
            .with_context(|| format!("读取 Node.js 缓存失败：{}", zip_path.display()))?;
        if expected_sha
            .as_deref()
            .is_none_or(|expected| compute_sha256_bytes(&bytes).eq_ignore_ascii_case(expected))
        {
            if let Some(progress) = progress {
                progress(bytes.len() as u64, Some(bytes.len() as u64));
            }
            return Ok(NodeRuntimePackage {
                source_url: zip_url,
                version: release.version,
                zip_name,
                cache_hit: true,
                bytes,
            });
        }
    }

    let bytes = download_bytes_with_progress(&zip_url, Duration::from_secs(240), progress, cancel)
        .context("下载 Node.js 运行时失败")?;
    check_download_cancel(cancel)?;
    if let Some(expected) = expected_sha {
        let actual = compute_sha256_bytes(&bytes);
        if !actual.eq_ignore_ascii_case(&expected) {
            bail!("Node.js 运行时校验失败：期望 {expected}，实际 {actual}");
        }
    }
    fs::write(&zip_path, &bytes)
        .with_context(|| format!("写入 Node.js 缓存失败：{}", zip_path.display()))?;
    Ok(NodeRuntimePackage {
        source_url: zip_url,
        version: release.version,
        zip_name,
        cache_hit: false,
        bytes,
    })
}

/// 将 Node.js zip 根目录内的文件解压为目标 nodejs 目录。
pub(crate) fn extract_node_runtime(package: &NodeRuntimePackage, target_dir: &Path) -> Result<()> {
    if target_dir.exists() {
        delete_path(target_dir)?;
    }
    fs::create_dir_all(target_dir)?;
    let mut archive = ZipArchive::new(Cursor::new(package.bytes.as_slice()))
        .context("无法读取 Node.js 运行时压缩包")?;
    let root_prefix = format!("node-{}-win-", package.version);
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let entry_name = entry.name().replace('\\', "/");
        if entry_name.ends_with('/') {
            continue;
        }
        let relative = entry_name
            .strip_prefix(&root_prefix)
            .and_then(|name| name.split_once('/').map(|(_, rest)| rest))
            .with_context(|| {
                format!(
                    "Node.js 运行时压缩包结构不符合预期：{} / {}",
                    package.zip_name, entry_name
                )
            })?;
        if relative.trim().is_empty() {
            continue;
        }
        let relative_path = Path::new(relative);
        if !is_safe_relative_path(relative_path) {
            bail!("Node.js 运行时压缩包包含不安全路径：{relative}");
        }
        let out_path = target_dir.join(relative_path);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output =
            File::create(&out_path).with_context(|| format!("创建 {}", out_path.display()))?;
        io::copy(&mut entry, &mut output)
            .with_context(|| format!("写入 {}", out_path.display()))?;
    }
    if !target_dir.join("node.exe").exists() {
        bail!("Node.js 运行时安装后未找到 node.exe。");
    }
    Ok(())
}

/// 将 GitHub 源码 zip 解压为目标 BoundaryMetaServer-main 目录。
pub(crate) fn extract_boundary_meta_server(
    package: &BoundaryMetaServerPackage,
    target_dir: &Path,
) -> Result<()> {
    if target_dir.exists() {
        delete_path(target_dir)?;
    }
    fs::create_dir_all(target_dir)?;
    let mut archive = ZipArchive::new(Cursor::new(package.bytes.as_slice()))
        .context("无法读取 BoundaryMetaServer 压缩包")?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let entry_name = entry.name().replace('\\', "/");
        if entry.is_dir() || entry_name.ends_with('/') {
            continue;
        }
        let Some(relative_path) = github_archive_relative_path(&entry_name) else {
            continue;
        };
        if !is_safe_relative_path(&relative_path) {
            bail!("BoundaryMetaServer 压缩包包含不安全路径：{entry_name}");
        }
        let out_path = target_dir.join(&relative_path);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output =
            File::create(&out_path).with_context(|| format!("创建 {}", out_path.display()))?;
        io::copy(&mut entry, &mut output)
            .with_context(|| format!("写入 {}", out_path.display()))?;
    }
    if !target_dir.join("index.js").exists() {
        bail!("BoundaryMetaServer 安装后未找到 index.js。");
    }
    Ok(())
}

fn is_safe_relative_path(path: &Path) -> bool {
    path.components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn boundary_meta_server_archive_valid(bytes: &[u8]) -> bool {
    let Ok(mut archive) = ZipArchive::new(Cursor::new(bytes)) else {
        return false;
    };
    (0..archive.len()).any(|index| {
        archive.by_index(index).ok().is_some_and(|entry| {
            github_archive_relative_path(&entry.name().replace('\\', "/")).is_some_and(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("index.js"))
            })
        })
    })
}

fn github_archive_relative_path(entry_name: &str) -> Option<PathBuf> {
    let normalized = Path::new(entry_name);
    let mut components = normalized.components();
    match components.next()? {
        Component::Normal(_) => {}
        _ => return None,
    }
    let relative = components.as_path();
    if relative.as_os_str().is_empty() {
        None
    } else {
        Some(relative.to_path_buf())
    }
}

fn latest_node_lts_release() -> Result<NodeRelease> {
    let text = download_text(NODEJS_DIST_INDEX_URL).context("请求 Node.js 版本索引失败")?;
    let releases =
        serde_json::from_str::<Vec<NodeRelease>>(&text).context("解析 Node.js 版本索引失败")?;
    releases
        .into_iter()
        .find(|release| release.lts != serde_json::Value::Bool(false))
        .context("Node.js 版本索引中没有找到 LTS 版本")
}

fn node_windows_arch() -> Result<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("x64"),
        "aarch64" => Ok("arm64"),
        "x86" => Ok("x86"),
        other => bail!("当前架构暂不支持自动下载 Node.js Windows 运行时：{other}"),
    }
}

fn parse_shasum_for_file(text: &str, file_name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let sha = parts.next()?;
        let name = parts.next()?;
        if name.eq_ignore_ascii_case(file_name)
            && sha.len() == 64
            && sha.chars().all(|ch| ch.is_ascii_hexdigit())
        {
            Some(sha.to_string())
        } else {
            None
        }
    })
}

fn download_text(url: &str) -> Result<String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(format!("boundary-toolbox/{}", APP_VERSION))
        .build()?
        .get(url)
        .send()
        .with_context(|| format!("请求失败：{url}"))?
        .error_for_status()
        .with_context(|| format!("服务器返回错误：{url}"))?
        .text()
        .with_context(|| format!("读取响应失败：{url}"))
}

fn download_bytes_with_progress(
    url: &str,
    timeout: Duration,
    progress: Option<DownloadProgress<'_>>,
    cancel: Option<&InstallCancelToken>,
) -> Result<Vec<u8>> {
    check_download_cancel(cancel)?;
    let mut response = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .user_agent(format!("boundary-toolbox/{}", APP_VERSION))
        .build()?
        .get(url)
        .send()
        .with_context(|| format!("请求失败：{url}"))?
        .error_for_status()
        .with_context(|| format!("服务器返回错误：{url}"))?;
    let total = response.content_length();
    let mut bytes = Vec::with_capacity(total.unwrap_or_default().min(usize::MAX as u64) as usize);
    let mut downloaded = 0_u64;
    let mut last_reported = 0_u64;
    if let Some(progress) = progress {
        progress(downloaded, total);
    }

    let mut buffer = [0_u8; 128 * 1024];
    loop {
        check_download_cancel(cancel)?;
        let read = response
            .read(&mut buffer)
            .with_context(|| format!("读取下载内容失败：{url}"))?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        downloaded += read as u64;
        if let Some(progress) = progress {
            let finished = total.is_some_and(|total| downloaded >= total);
            if finished || downloaded.saturating_sub(last_reported) >= 1024 * 1024 {
                progress(downloaded, total);
                last_reported = downloaded;
            }
        }
    }
    check_download_cancel(cancel)?;
    if let Some(progress) = progress {
        if downloaded != last_reported {
            progress(downloaded, total);
        }
    }
    Ok(bytes)
}

fn check_download_cancel(cancel: Option<&InstallCancelToken>) -> Result<()> {
    if cancel.is_some_and(InstallCancelToken::is_cancelled) {
        bail!("安装已取消");
    }
    Ok(())
}

/// 预检在线 zip，并把所有必需文件读入内存。
///
/// 替换前先把字节保存在内存里，可以防止代理返回 HTML、损坏 zip、
/// 或 release 缺少文件时留下半安装状态。
pub(crate) fn read_project_rebound_release_files(
    source_url: &str,
    release_zip: &[u8],
) -> Result<HashMap<String, Vec<u8>>> {
    if !looks_like_zip(release_zip) {
        bail!(
            "ProjectRebound Nightly Release 下载结果不是 zip：{}。这通常是下载代理返回了网页或错误页，请在设置里换一个延迟可用的代理节点，或清空代理后直连。",
            describe_download_payload(source_url, release_zip)
        );
    }
    let mut archive = ZipArchive::new(Cursor::new(release_zip))
        .context("无法读取 ProjectRebound Nightly Release 压缩包")?;
    let mut files = HashMap::new();
    for item_name in PROJECT_REBOUND_ONLINE_FILES {
        let bytes = read_project_rebound_release_file(&mut archive, item_name)?;
        files.insert(item_name.to_string(), bytes);
    }
    Ok(files)
}

fn looks_like_zip(bytes: &[u8]) -> bool {
    bytes.starts_with(&[b'P', b'K', 0x03, 0x04])
        || bytes.starts_with(&[b'P', b'K', 0x05, 0x06])
        || bytes.starts_with(&[b'P', b'K', 0x07, 0x08])
}

fn describe_download_payload(source_url: &str, bytes: &[u8]) -> String {
    let head = bytes
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "url={source_url}, length={} bytes, head={head}",
        bytes.len()
    )
}

/// 在 ProjectRebound zip 内按文件名查找一个必需文件。
fn read_project_rebound_release_file(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    item_name: &str,
) -> Result<Vec<u8>> {
    let entry_name = archive
        .file_names()
        .find(|name| {
            Path::new(name).file_name().is_some_and(|file_name| {
                file_name.to_string_lossy().eq_ignore_ascii_case(item_name)
            })
        })
        .map(ToOwned::to_owned)
        .with_context(|| format!("ProjectRebound Nightly Release 缺少 {}", item_name))?;
    let mut entry = archive
        .by_name(&entry_name)
        .with_context(|| format!("无法读取 ProjectRebound 文件 {}", entry_name))?;
    if entry.is_dir() {
        bail!("ProjectRebound Nightly Release 中的 {} 不是文件", item_name);
    }
    let mut bytes = Vec::new();
    entry
        .read_to_end(&mut bytes)
        .with_context(|| format!("读取 ProjectRebound 文件 {} 失败", entry_name))?;
    if bytes.is_empty() {
        bail!("ProjectRebound Nightly Release 中的 {} 是空文件", item_name);
    }
    Ok(bytes)
}

/// 将已校验的 ProjectRebound 文件写入最终目标路径。
pub(crate) fn write_project_rebound_release_item(
    files: &HashMap<String, Vec<u8>>,
    item_name: &str,
    target_path: &Path,
) -> Result<()> {
    let bytes = files
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(item_name))
        .map(|(_, bytes)| bytes.as_slice())
        .with_context(|| format!("ProjectRebound Nightly Release 缺少 {}", item_name))?;
    replace_file_with_bytes(target_path, bytes)
        .with_context(|| format!("写入在线 ProjectRebound 文件 {} 失败", item_name))
}

/// 写入同目录临时文件后，原子替换最终文件。
fn replace_file_with_bytes(target_path: &Path, bytes: &[u8]) -> Result<()> {
    if target_path.exists() && target_path.is_dir() {
        bail!("目标路径是目录，无法按文件替换：{}", target_path.display());
    }
    let parent = target_path
        .parent()
        .with_context(|| format!("目标路径缺少父目录：{}", target_path.display()))?;
    fs::create_dir_all(parent)?;
    let temp_path = temp_file_for_target(target_path)?;
    {
        let mut output = File::create(&temp_path)
            .with_context(|| format!("创建临时文件失败：{}", temp_path.display()))?;
        output
            .write_all(bytes)
            .with_context(|| format!("写入临时文件失败：{}", temp_path.display()))?;
        output
            .sync_all()
            .with_context(|| format!("刷新临时文件失败：{}", temp_path.display()))?;
    }

    // 使用带 WRITE_THROUGH 的 MoveFileExW，比 remove+rename 更可靠，
    // 普通失败时不会留下目标文件缺失状态。
    if let Err(error) = move_file_replace(&temp_path, target_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    Ok(())
}

/// 在目标旁边生成唯一临时路径，确保替换发生在同一卷内。
fn temp_file_for_target(target_path: &Path) -> Result<PathBuf> {
    let parent = target_path
        .parent()
        .with_context(|| format!("目标路径缺少父目录：{}", target_path.display()))?;
    let file_name = target_path
        .file_name()
        .with_context(|| format!("目标路径缺少文件名：{}", target_path.display()))?
        .to_string_lossy();
    Ok(parent.join(format!(".{}.{}.tmp", file_name, Uuid::new_v4().simple())))
}

/// 执行 Windows 原子替换。
fn move_file_replace(source: &Path, target: &Path) -> Result<()> {
    let source_wide = path_to_wide(source);
    let target_wide = path_to_wide(target);
    unsafe {
        MoveFileExW(
            PCWSTR(source_wide.as_ptr()),
            PCWSTR(target_wide.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .with_context(|| format!("替换文件失败：{} -> {}", source.display(), target.display()))
}

/// 将 Rust 路径转换为以 0 结尾的 UTF-16 Windows 字符串。
fn path_to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// 流式计算文件 SHA-256，避免完整载入大文件。
fn compute_file_sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn compute_sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// 为安装记录采集大小、哈希和文件数量元数据。
pub(crate) fn collect_stats(path: &Path) -> Result<ItemStats> {
    if path.is_file() {
        return Ok(ItemStats {
            size: path.metadata()?.len(),
            sha256: Some(compute_file_sha256(path)?),
            file_count: None,
            dir_count: None,
        });
    }

    let mut total_size = 0_u64;
    let mut file_count = 0_u64;
    let mut dir_count = 0_u64;
    for entry in WalkDir::new(path) {
        let entry = entry?;
        if entry.file_type().is_file() {
            file_count += 1;
            total_size += entry.metadata()?.len();
        } else if entry.file_type().is_dir() && entry.path() != path {
            dir_count += 1;
        }
    }
    Ok(ItemStats {
        size: total_size,
        sha256: None,
        file_count: Some(file_count),
        dir_count: Some(dir_count),
    })
}
