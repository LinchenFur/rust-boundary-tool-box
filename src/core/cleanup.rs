//! 旧模组残留和 Engine.ini 冲突配置清理。

use super::filesystem::delete_path;
use super::*;

/// 清理 Engine.ini 后折叠连续空行。
fn normalize_blank_lines(lines: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    let mut previous_blank = false;
    for line in lines {
        let blank = line.trim().is_empty();
        if blank && previous_blank {
            continue;
        }
        normalized.push(line.clone());
        previous_blank = blank;
    }
    while normalized
        .first()
        .is_some_and(|line| line.trim().is_empty())
    {
        normalized.remove(0);
    }
    while normalized.last().is_some_and(|line| line.trim().is_empty()) {
        normalized.pop();
    }
    normalized
}

/// 从目录中移除已知子项，并返回已删除路径。
fn remove_known_children(
    base_dir: &Path,
    known_names: &[&str],
    logger: Logger,
) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    if !base_dir.is_dir() {
        return Ok(removed);
    }
    let known: HashSet<&str> = known_names.iter().copied().collect();
    for entry in fs::read_dir(base_dir)? {
        let entry = entry?;
        let child = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !known.contains(name.as_ref()) {
            continue;
        }
        logger(format!(
            "[{}] 清理旧模组残留：{}",
            now_text(),
            child.display()
        ));
        delete_path(&child)?;
        removed.push(child.display().to_string());
    }
    if base_dir.exists() && fs::read_dir(base_dir)?.next().is_none() {
        logger(format!(
            "[{}] 删除空目录：{}",
            now_text(),
            base_dir.display()
        ));
        fs::remove_dir(base_dir)?;
        removed.push(base_dir.display().to_string());
    }
    Ok(removed)
}

/// 清理会和 Rust 工具箱冲突的旧一键包残留。
pub(crate) fn clean_legacy_range_mod(target_win64: &Path, logger: Logger) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    let project_boundary_dir = target_win64
        .parent()
        .and_then(Path::parent)
        .context("无法定位 ProjectBoundary 目录")?;
    let logic_mods_dir = project_boundary_dir
        .join("Content")
        .join("Paks")
        .join("LogicMods");
    for name in OLD_MOD_LOGICMOD_FILES {
        let path = logic_mods_dir.join(name);
        if path.exists() {
            logger(format!(
                "[{}] 清理旧模组残留：{}",
                now_text(),
                path.display()
            ));
            delete_path(&path)?;
            removed.push(path.display().to_string());
        }
    }
    if logic_mods_dir.is_dir() && fs::read_dir(&logic_mods_dir)?.next().is_none() {
        logger(format!(
            "[{}] 删除空目录：{}",
            now_text(),
            logic_mods_dir.display()
        ));
        fs::remove_dir(&logic_mods_dir)?;
        removed.push(logic_mods_dir.display().to_string());
    }

    let root_signature_removed = remove_known_children(
        &target_win64.join("Mods"),
        OLD_MOD_UE4SS_MOD_ENTRIES,
        logger.clone(),
    )?;
    let nested_ue4ss_dir = target_win64.join("ue4ss");
    let nested_signature_removed = remove_known_children(
        &nested_ue4ss_dir.join("Mods"),
        OLD_MOD_UE4SS_MOD_ENTRIES,
        logger.clone(),
    )?;
    let mut nested_known = OLD_MOD_UE4SS_SUPPORT_FILES.to_vec();
    nested_known.extend_from_slice(OLD_MOD_UE4SS_LOADER_FILES);
    let nested_support_removed =
        remove_known_children(&nested_ue4ss_dir, &nested_known, logger.clone())?;

    let root_support_present = OLD_MOD_UE4SS_SUPPORT_FILES
        .iter()
        .any(|name| target_win64.join(name).exists());
    let root_signature_found = !root_signature_removed.is_empty();
    let nested_signature_found =
        !nested_signature_removed.is_empty() || !nested_support_removed.is_empty();

    removed.extend(root_signature_removed);
    if root_signature_found || root_support_present {
        removed.extend(remove_known_children(
            target_win64,
            OLD_MOD_UE4SS_SUPPORT_FILES,
            logger.clone(),
        )?);
    }
    if root_signature_found || root_support_present {
        for name in OLD_MOD_UE4SS_LOADER_FILES {
            let path = target_win64.join(name);
            if path.exists() {
                logger(format!(
                    "[{}] 清理旧模组加载器：{}",
                    now_text(),
                    path.display()
                ));
                delete_path(&path)?;
                removed.push(path.display().to_string());
            }
        }
    }

    if nested_signature_found {
        removed.extend(nested_signature_removed);
        removed.extend(nested_support_removed);
        if nested_ue4ss_dir.is_dir() && fs::read_dir(&nested_ue4ss_dir)?.next().is_none() {
            logger(format!(
                "[{}] 删除空目录：{}",
                now_text(),
                nested_ue4ss_dir.display()
            ));
            fs::remove_dir(&nested_ue4ss_dir)?;
            removed.push(nested_ue4ss_dir.display().to_string());
        }
    }
    Ok(removed)
}

/// 移除会破坏本地社区服登录的旧 OnlineSubsystem 配置。
pub(crate) fn clean_engine_ini(logger: Logger) -> Result<Option<PathBuf>> {
    let local_appdata = env::var("LOCALAPPDATA").context("未找到 LOCALAPPDATA")?;
    let engine_ini = PathBuf::from(local_appdata)
        .join("ProjectBoundary")
        .join("Saved")
        .join("Config")
        .join("WindowsClient")
        .join("Engine.ini");
    if !engine_ini.exists() {
        logger(format!(
            "[{}] Engine.ini 不存在，跳过：{}",
            now_text(),
            engine_ini.display()
        ));
        return Ok(None);
    }

    let content = fs::read_to_string(&engine_ini).unwrap_or_default();
    let lines: Vec<String> = content.lines().map(ToOwned::to_owned).collect();
    let header_re = Regex::new(r"^\[(?P<section>[^\]]+)\]\s*$")?;
    let key_re =
        Regex::new(r"(?i)^DefaultPlatformService\s*=\s*<Default Platform Identifier>\s*$")?;
    let mut output = Vec::new();
    let mut current_section: Option<String> = None;
    let mut section_body: Vec<String> = Vec::new();
    let mut removed = false;

    // 按 section 重建文件，确保无关用户设置和原有顺序在清理后仍保留。
    let mut flush_section =
        |section_name: Option<String>, section_lines: &[String], output_lines: &mut Vec<String>| {
            if section_name.as_deref() != Some("OnlineSubsystem") {
                if let Some(name) = section_name {
                    output_lines.push(format!("[{}]", name));
                }
                output_lines.extend(section_lines.iter().cloned());
                return;
            }

            let mut kept = Vec::new();
            let mut removed_here = false;
            for line in section_lines {
                if key_re.is_match(line.trim()) {
                    removed_here = true;
                    removed = true;
                    continue;
                }
                kept.push(line.clone());
            }

            if kept.iter().any(|line| !line.trim().is_empty()) {
                output_lines.push("[OnlineSubsystem]".to_string());
                output_lines.extend(kept);
                if removed_here {
                    logger(format!(
                        "[{}] 已删除 [OnlineSubsystem] 节内的冲突键值。",
                        now_text()
                    ));
                }
                return;
            }

            if removed_here || !section_lines.is_empty() {
                removed = true;
                logger(format!(
                    "[{}] 已删除 [OnlineSubsystem] 冲突节。",
                    now_text()
                ));
            }
        };

    for line in lines {
        if let Some(captures) = header_re.captures(line.trim()) {
            flush_section(current_section.take(), &section_body, &mut output);
            current_section = Some(captures["section"].to_string());
            section_body.clear();
        } else {
            section_body.push(line);
        }
    }
    flush_section(current_section.take(), &section_body, &mut output);

    let normalized = normalize_blank_lines(&output);
    let mut new_content = normalized.join("\r\n");
    if !new_content.is_empty() {
        new_content.push_str("\r\n");
    }
    if removed && new_content != content {
        fs::write(&engine_ini, new_content)?;
        logger(format!(
            "[{}] Engine.ini 已清理：{}",
            now_text(),
            engine_ini.display()
        ));
        return Ok(Some(engine_ini));
    }
    logger(format!(
        "[{}] Engine.ini 未发现需要清理的冲突配置。",
        now_text()
    ));
    Ok(None)
}
