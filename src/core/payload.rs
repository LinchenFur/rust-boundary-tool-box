//! 内嵌载荷、在线 ProjectRebound 更新和安装记录统计。

use super::*;

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

/// 下载当前 ProjectRebound Nightly zip。
pub(crate) fn download_project_rebound_release() -> Result<Vec<u8>> {
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(90))
        .user_agent(format!("boundary-toolbox/{}", APP_VERSION))
        .build()?
        .get(PROJECT_REBOUND_RELEASE_URL)
        .send()
        .context("下载 ProjectRebound Nightly Release 失败")?
        .error_for_status()
        .context("ProjectRebound Nightly Release 返回错误状态")?;
    let bytes = response
        .bytes()
        .context("读取 ProjectRebound Nightly Release 内容失败")?;
    Ok(bytes.to_vec())
}

/// 预检在线 zip，并把所有必需文件读入内存。
///
/// 替换前先把字节保存在内存里，可以防止代理返回 HTML、损坏 zip、
/// 或 release 缺少文件时留下半安装状态。
pub(crate) fn read_project_rebound_release_files(
    release_zip: &[u8],
) -> Result<HashMap<String, Vec<u8>>> {
    let mut archive = ZipArchive::new(Cursor::new(release_zip))
        .context("无法读取 ProjectRebound Nightly Release 压缩包")?;
    let mut files = HashMap::new();
    for item_name in PROJECT_REBOUND_ONLINE_FILES {
        let bytes = read_project_rebound_release_file(&mut archive, item_name)?;
        files.insert(item_name.to_string(), bytes);
    }
    Ok(files)
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
