//! Steam 启动前状态检查。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use regex::Regex;
use sysinfo::System;

use super::pathing::steam_registry_paths;
use super::util::hidden_command;
use super::*;

#[derive(Debug, Clone)]
struct SteamProcess {
    pid: u32,
    exe: Option<PathBuf>,
}

impl InstallerCore {
    /// 启动游戏前确认 Steam 已经在后台运行，并且没有处于离线模式。
    ///
    /// Steam 没有运行或配置要求离线模式时会尝试拉起 Steam，然后中断本次启动；
    /// 用户需要等 Steam 登录在线后再次点击启动，避免游戏被无 Steam 会话拉起。
    pub fn ensure_steam_ready_for_launch(&self) -> Result<()> {
        let processes = collect_steam_processes();
        let roots = steam_roots(&processes);
        let steam_exe = find_steam_exe(&processes, &roots);

        if processes.is_empty() {
            self.start_steam_or_report(steam_exe.as_deref(), "Steam 未在后台运行")?;
            bail!("Steam 未在后台运行，已尝试拉起 Steam。请等待 Steam 登录并在线后再启动游戏。");
        }

        if let Some(report) = offline_config_report(&roots) {
            self.start_steam_or_report(steam_exe.as_deref(), "Steam 当前处于离线模式")?;
            bail!(
                "Steam 当前疑似处于离线模式，已尝试拉起 Steam。\n{}\n请在 Steam 中切换到在线状态后再启动游戏。",
                report
            );
        }

        let pids = processes
            .iter()
            .map(|process| process.pid.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        self.log(format!(
            "Steam 状态检查通过：steam.exe 正在运行，未检测到离线模式，PID {pids}。"
        ));
        Ok(())
    }

    fn start_steam_or_report(&self, steam_exe: Option<&Path>, reason: &str) -> Result<()> {
        let Some(steam_exe) = steam_exe else {
            bail!("{reason}，并且未找到 steam.exe。请先手动启动 Steam 并登录在线后再启动游戏。");
        };

        self.log(format!("{reason}，尝试拉起 Steam：{}", steam_exe.display()));
        hidden_command(steam_exe)
            .spawn()
            .with_context(|| format!("拉起 Steam 失败：{}", steam_exe.display()))?;
        Ok(())
    }
}

/// 收集正在运行的 steam.exe。steamwebhelper.exe 不代表主客户端已就绪，因此不纳入。
fn collect_steam_processes() -> Vec<SteamProcess> {
    let mut system = System::new_all();
    system.refresh_all();
    system
        .processes()
        .values()
        .filter(|process| {
            process
                .name()
                .to_string_lossy()
                .eq_ignore_ascii_case("steam.exe")
        })
        .map(|process| SteamProcess {
            pid: process.pid().as_u32(),
            exe: process.exe().map(Path::to_path_buf),
        })
        .collect()
}

/// 汇总 Steam 安装根目录，优先取正在运行的 steam.exe，再取注册表和默认安装目录。
fn steam_roots(processes: &[SteamProcess]) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();

    for process in processes {
        if let Some(parent) = process.exe.as_deref().and_then(Path::parent) {
            push_unique_path(&mut roots, &mut seen, parent.to_path_buf());
        }
    }

    for path in steam_registry_paths() {
        push_unique_path(&mut roots, &mut seen, path);
    }

    for path in [
        PathBuf::from(r"C:\Program Files (x86)\Steam"),
        PathBuf::from(r"C:\Program Files\Steam"),
    ] {
        push_unique_path(&mut roots, &mut seen, path);
    }

    roots
}

fn push_unique_path(paths: &mut Vec<PathBuf>, seen: &mut HashSet<String>, path: PathBuf) {
    let key = path.to_string_lossy().replace('/', "\\").to_lowercase();
    if seen.insert(key) {
        paths.push(path);
    }
}

/// 找到可用于拉起 Steam 的 steam.exe。
fn find_steam_exe(processes: &[SteamProcess], roots: &[PathBuf]) -> Option<PathBuf> {
    for process in processes {
        if let Some(exe) = process.exe.as_ref().filter(|path| path.exists()) {
            return Some(exe.clone());
        }
    }
    for root in roots {
        let candidate = root.join("steam.exe");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// 读取 Steam 配置中的离线模式标记。读不到配置时不阻断启动，只按运行状态判断。
fn offline_config_report(roots: &[PathBuf]) -> Option<String> {
    for root in roots {
        for path in offline_config_paths(root) {
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            if steam_config_requests_offline(&content) {
                return Some(format!("检测到离线配置：{}", path.display()));
            }
        }
    }
    None
}

fn offline_config_paths(root: &Path) -> Vec<PathBuf> {
    let mut paths = vec![
        root.join("config").join("loginusers.vdf"),
        root.join("config").join("config.vdf"),
    ];

    let userdata = root.join("userdata");
    if let Ok(entries) = fs::read_dir(userdata) {
        for entry in entries.flatten() {
            let local_config = entry.path().join("config").join("localconfig.vdf");
            paths.push(local_config);
        }
    }

    paths
}

/// 判断 VDF 文本是否显式要求 Steam 离线模式。
fn steam_config_requests_offline(content: &str) -> bool {
    let bool_flags = ["WantsOfflineMode", "Offline", "ForceOfflineMode"];
    bool_flags
        .iter()
        .any(|flag| vdf_bool_flag_enabled(content, flag))
        || vdf_string_flag_equals(content, "StartupMode", "Offline")
}

fn vdf_bool_flag_enabled(content: &str, flag: &str) -> bool {
    let pattern = format!(r#""{}"\s+"?1"?"#, regex::escape(flag));
    Regex::new(&format!("(?i){pattern}"))
        .map(|regex| regex.is_match(content))
        .unwrap_or(false)
}

fn vdf_string_flag_equals(content: &str, key: &str, value: &str) -> bool {
    let pattern = format!(r#""{}"\s+"{}""#, regex::escape(key), regex::escape(value));
    Regex::new(&format!("(?i){pattern}"))
        .map(|regex| regex.is_match(content))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_wants_offline_mode() {
        let content = r#""WantsOfflineMode" "1""#;
        assert!(steam_config_requests_offline(content));
    }

    #[test]
    fn ignores_online_mode() {
        let content = r#""WantsOfflineMode" "0""#;
        assert!(!steam_config_requests_offline(content));
    }

    #[test]
    fn detects_startup_offline_mode_case_insensitively() {
        let content = r#""startupmode" "offline""#;
        assert!(steam_config_requests_offline(content));
    }
}
