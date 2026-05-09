//! 游戏目录识别、Steam 元数据读取和全盘扫描。

use super::*;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use anyhow::{Result, bail};
use regex::Regex;
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

/// 列出当前 Windows 上存在的盘符根目录。
pub fn list_available_drives() -> Vec<PathBuf> {
    ('A'..='Z')
        .map(|letter| PathBuf::from(format!("{}:\\", letter)))
        .filter(|path| path.exists())
        .collect()
}

/// 在单个盘符下深度优先扫描游戏主程序。
fn scan_drive_for_game(drive_root: PathBuf, stop_flag: Arc<AtomicBool>) -> Option<PathBuf> {
    let mut stack = vec![drive_root];
    let skip_names: HashSet<&str> = ["$recycle.bin", "system volume information"]
        .into_iter()
        .collect();
    while let Some(current) = stack.pop() {
        if stop_flag.load(Ordering::Relaxed) {
            return None;
        }
        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        let mut child_dirs = Vec::new();
        for entry in entries.flatten() {
            if stop_flag.load(Ordering::Relaxed) {
                return None;
            }
            let path = entry.path();
            if path.is_file()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(GAME_EXE))
            {
                let candidate = path.parent()?.to_path_buf();
                if validate_win64_path(&candidate).is_ok() {
                    return Some(candidate);
                }
            } else if path.is_dir() {
                let name = entry.file_name().to_string_lossy().to_lowercase();
                // 跳过常见系统目录；这些目录通常会拒绝访问，正常安装也不会包含 Steam 库。
                if skip_names.contains(name.as_str()) {
                    continue;
                }
                child_dirs.push(path);
            }
        }
        child_dirs.reverse();
        stack.extend(child_dirs);
    }
    None
}

/// 并发扫描多个盘符，命中第一个结果后通知其它工作线程停止。
pub(crate) fn scan_drives_for_game(drives: &[PathBuf], logger: Logger) -> Option<PathBuf> {
    if drives.is_empty() {
        return None;
    }
    let stop_flag = Arc::new(AtomicBool::new(false));
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut handles = Vec::new();
    for drive in drives.iter().cloned() {
        logger(format!(
            "[{}] 开始扫描盘符：{}",
            now_text(),
            drive.display()
        ));
        let stop = stop_flag.clone();
        let tx = tx.clone();
        handles.push(thread::spawn(move || {
            let result = scan_drive_for_game(drive.clone(), stop);
            let _ = tx.send((drive, result));
        }));
    }
    drop(tx);

    let mut found = None;
    for (drive, result) in &rx {
        if let Some(path) = result {
            logger(format!(
                "[{}] 扫描命中：{} -> {}",
                now_text(),
                drive.display(),
                path.display()
            ));
            stop_flag.store(true, Ordering::Relaxed);
            found = Some(path);
            break;
        }
        logger(format!(
            "[{}] 扫描完成，未找到游戏目录：{}",
            now_text(),
            drive.display()
        ));
    }

    for handle in handles {
        let _ = handle.join();
    }
    found
}

/// 校验路径是否为 Boundary 的精确 Binaries\Win64 目录。
pub fn validate_win64_path(path: &Path) -> Result<()> {
    if !path.exists() {
        bail!("目录不存在：{}", path.display());
    }
    if !path.is_dir() {
        bail!("不是目录：{}", path.display());
    }
    if !path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("Win64"))
    {
        bail!("目标目录不是 Win64。");
    }
    let exe_path = path.join(GAME_EXE);
    if !exe_path.exists() {
        bail!("未找到游戏主程序：{}", exe_path.display());
    }
    Ok(())
}

/// 将用户选择的各种游戏根目录形式解析为 Binaries\Win64。
pub fn normalize_selected_path(raw_path: &Path) -> Result<PathBuf> {
    let selected = raw_path
        .canonicalize()
        .unwrap_or_else(|_| raw_path.to_path_buf());
    let candidates = [
        selected.clone(),
        selected
            .join("ProjectBoundary")
            .join("Binaries")
            .join("Win64"),
        selected.join("Binaries").join("Win64"),
    ];
    for candidate in candidates {
        if validate_win64_path(&candidate).is_ok() {
            return Ok(candidate);
        }
    }
    bail!(
        "请选择 Boundary 游戏根目录、ProjectBoundary 目录，或 ProjectBoundary\\Binaries\\Win64 目录。"
    )
}

/// 从 HKCU/HKLM 注册表键读取常见 Steam 安装根目录。
pub(crate) fn steam_registry_paths() -> Vec<PathBuf> {
    let mut results = Vec::new();
    let registry_candidates = [
        (HKEY_CURRENT_USER, r"Software\Valve\Steam", "SteamPath"),
        (HKEY_CURRENT_USER, r"Software\Valve\Steam", "SteamExe"),
        (
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\WOW6432Node\Valve\Steam",
            "InstallPath",
        ),
        (HKEY_LOCAL_MACHINE, r"SOFTWARE\Valve\Steam", "InstallPath"),
    ];
    for (root, subkey, value_name) in registry_candidates {
        let key = match RegKey::predef(root).open_subkey(subkey) {
            Ok(key) => key,
            Err(_) => continue,
        };
        let value: String = match key.get_value(value_name) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let mut path = PathBuf::from(value.replace('/', "\\"));
        if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
        {
            path.pop();
        }
        results.push(path);
    }
    results
}

/// 构造 Steam libraryfolders.vdf 的候选路径列表。
fn candidate_libraryfolders_files() -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    for steam_root in steam_registry_paths() {
        let path = steam_root.join("steamapps").join("libraryfolders.vdf");
        let key = path.to_string_lossy().to_lowercase();
        if seen.insert(key) {
            candidates.push(path);
        }
    }
    for path in [
        PathBuf::from(r"C:\Program Files (x86)\Steam\steamapps\libraryfolders.vdf"),
        PathBuf::from(r"C:\Program Files\Steam\steamapps\libraryfolders.vdf"),
    ] {
        let key = path.to_string_lossy().to_lowercase();
        if seen.insert(key) {
            candidates.push(path);
        }
    }
    candidates
}

/// 用小范围正则解析 Steam VDF 中的库路径。
fn parse_library_paths(libraryfolders_path: &Path) -> Result<Vec<PathBuf>> {
    let content = fs::read_to_string(libraryfolders_path)?;
    let regex = Regex::new(r#""path"\s+"([^"]+)""#)?;
    Ok(regex
        .captures_iter(&content)
        .map(|captures| PathBuf::from(captures[1].replace("\\\\", "\\")))
        .collect())
}

/// 当 Boundary appmanifest 存在时读取其安装目录。
fn read_manifest_install_dir(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)?;
    let regex = Regex::new(r#""installdir"\s+"([^"]+)""#)?;
    Ok(regex
        .captures(&content)
        .map(|captures| captures[1].to_string()))
}

/// 根据 Steam 元数据检测 Boundary 的 Win64 路径。
pub(crate) fn detect_steam_game_win64() -> Result<(PathBuf, String)> {
    let mut errors = Vec::new();
    for libraryfolders_path in candidate_libraryfolders_files() {
        if !libraryfolders_path.exists() {
            errors.push(format!(
                "未找到 Steam 库配置：{}",
                libraryfolders_path.display()
            ));
            continue;
        }
        let library_paths = match parse_library_paths(&libraryfolders_path) {
            Ok(paths) => paths,
            Err(error) => {
                errors.push(format!(
                    "读取失败 {}: {}",
                    libraryfolders_path.display(),
                    error
                ));
                continue;
            }
        };
        for library_root in library_paths {
            let manifest = library_root
                .join("steamapps")
                .join(format!("appmanifest_{}.acf", APP_ID));
            let Some(install_dir) = read_manifest_install_dir(&manifest)? else {
                continue;
            };
            let win64_path = library_root
                .join("steamapps")
                .join("common")
                .join(install_dir)
                .join("ProjectBoundary")
                .join("Binaries")
                .join("Win64");
            match validate_win64_path(&win64_path) {
                Ok(()) => {
                    return Ok((
                        win64_path.clone(),
                        format!("已通过 Steam 自动识别：{}", win64_path.display()),
                    ));
                }
                Err(error) => {
                    errors.push(format!("{} 指向的目录无效：{}", manifest.display(), error));
                }
            }
        }
    }
    if errors.is_empty() {
        errors.push(format!(
            "未在 Steam 库中找到 App ID {}（Boundary）。",
            APP_ID
        ));
    }
    bail!(errors.join("\n"))
}
