//! 运行进程识别、端口占用诊断和进程结束。

use super::util::hidden_taskkill_command;
use super::*;

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use netstat2::{AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, TcpState, get_sockets_info};
use sysinfo::System;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};

/// 为弹窗和日志生成紧凑的进程摘要字符串。
pub fn summarize_runtime_processes(snapshot: &RuntimeSnapshot) -> String {
    let parts = [
        ("游戏", &snapshot.game),
        ("服务包装器", &snapshot.wrapper),
        ("登录服务器", &snapshot.server),
    ]
    .into_iter()
    .map(|(label, items)| {
        if items.is_empty() {
            format!("{label} 0 个")
        } else {
            let details = items
                .iter()
                .map(format_runtime_process)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{label} {} 个（{}）", items.len(), details)
        }
    })
    .collect::<Vec<_>>();
    parts.join("；")
}

/// 格式化单个进程，包含 PID 以及 exe 路径或命令行。
fn format_runtime_process(process: &RuntimeProcess) -> String {
    let name = if process.name.trim().is_empty() {
        "未知进程"
    } else {
        process.name.trim()
    };
    let detail = if !process.exe.trim().is_empty() {
        process.exe.trim()
    } else {
        process.cmd.trim()
    };
    if detail.is_empty() {
        format!("{} PID {}", name, process.pid)
    } else {
        format!(
            "{} PID {} @ {}",
            name,
            process.pid,
            shorten_runtime_detail(detail)
        )
    }
}

/// 缩短长命令行，避免 UI 弹窗难以阅读。
fn shorten_runtime_detail(value: &str) -> String {
    let value = value.trim();
    if value.chars().count() <= 96 {
        return value.to_string();
    }
    let mut shortened = value.chars().take(93).collect::<String>();
    shortened.push_str("...");
    shortened
}

/// 对多个运行时进程组中的 PID 去重。
pub(crate) fn runtime_process_pids(snapshot: &RuntimeSnapshot) -> Vec<u32> {
    let mut pids = Vec::new();
    // 先停可能拉起其它进程的包装器/登录服务器，再停游戏本体。
    for group in [&snapshot.wrapper, &snapshot.server, &snapshot.game] {
        for process in group.iter() {
            if process.pid > 0 && !pids.contains(&process.pid) {
                pids.push(process.pid);
            }
        }
    }
    pids
}

/// 按专用镜像名收集 PID；用于诊断页兜底关闭已脱离目标路径匹配的游戏进程。
pub(crate) fn runtime_image_pids(image_names: &[&str]) -> Vec<u32> {
    let mut pids = Vec::new();
    for image_name in image_names {
        for pid in process_pids_by_image(image_name) {
            if pid > 0 && !pids.contains(&pid) {
                pids.push(pid);
            }
        }
    }
    pids
}

/// 为确认弹窗和错误信息格式化端口占用列表。
pub fn format_port_conflicts(conflicts: &[PortConflict]) -> String {
    conflicts
        .iter()
        .map(|item| {
            format!(
                "- {}/{} -> PID {} {} ({})",
                item.protocol.to_uppercase(),
                item.port,
                item.pid,
                item.name,
                item.exe
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 读取系统 socket 表，并筛选本地服务需要的端口。
pub(crate) fn collect_port_conflicts() -> Result<Vec<PortConflict>> {
    let sockets = get_sockets_info(
        AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6,
        ProtocolFlags::TCP | ProtocolFlags::UDP,
    )?;
    let mut system = System::new_all();
    system.refresh_all();
    let mut conflicts = Vec::new();

    for socket in sockets {
        match socket.protocol_socket_info {
            ProtocolSocketInfo::Tcp(tcp) => {
                if tcp.state != TcpState::Listen || !REQUIRED_TCP_PORTS.contains(&tcp.local_port) {
                    continue;
                }
                append_conflicts(
                    "TCP",
                    tcp.local_port,
                    &socket.associated_pids,
                    &system,
                    &mut conflicts,
                );
            }
            ProtocolSocketInfo::Udp(udp) => {
                if !REQUIRED_UDP_PORTS.contains(&udp.local_port) {
                    continue;
                }
                append_conflicts(
                    "UDP",
                    udp.local_port,
                    &socket.associated_pids,
                    &system,
                    &mut conflicts,
                );
            }
        }
    }

    conflicts.sort_by(|left, right| {
        (left.protocol.as_str(), left.port, left.pid).cmp(&(
            right.protocol.as_str(),
            right.port,
            right.pid,
        ))
    });
    conflicts.dedup_by(|left, right| {
        left.protocol == right.protocol && left.port == right.port && left.pid == right.pid
    });
    Ok(conflicts)
}

/// 追加端口冲突行，并兼容缺少进程元数据的 socket。
fn append_conflicts(
    protocol: &str,
    port: u16,
    pids: &[u32],
    system: &System,
    conflicts: &mut Vec<PortConflict>,
) {
    if pids.is_empty() {
        conflicts.push(PortConflict {
            protocol: protocol.to_string(),
            port,
            pid: 0,
            name: "未知进程".to_string(),
            exe: "未知路径".to_string(),
            expected: false,
        });
        return;
    }

    for pid in pids {
        let process = system.process(sysinfo::Pid::from_u32(*pid));
        conflicts.push(PortConflict {
            protocol: protocol.to_string(),
            port,
            pid: *pid,
            name: process
                .map(|process| process.name().to_string_lossy().into_owned())
                .unwrap_or_else(|| "未知进程".to_string()),
            exe: process
                .and_then(|process| process.exe())
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "未知路径".to_string()),
            expected: false,
        });
    }
}

/// 结束指定 PID，并把任何 taskkill 失败都作为用户可见错误。
pub(crate) fn kill_pids(pids: &[u32]) -> Result<()> {
    let failures = taskkill_pids(pids)?;
    if !failures.is_empty() {
        bail!("结束进程失败：\n{}", failures.join("\n"));
    }
    Ok(())
}

/// 逐个 PID 执行 taskkill，并返回详细失败信息而不是吞掉错误。
pub(crate) fn taskkill_pids(pids: &[u32]) -> Result<Vec<String>> {
    let mut failures = Vec::new();
    for pid in pids {
        let output = hidden_taskkill_command()
            .arg("/PID")
            .arg(pid.to_string())
            .arg("/T")
            .arg("/F")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("结束 PID {} 失败", pid))?;

        if !process_exists(*pid) {
            continue;
        }

        match terminate_pid(*pid) {
            Ok(()) => continue,
            Err(error) if !process_exists(*pid) => {
                let _ = error;
                continue;
            }
            Err(error) => {
                let detail = if output.status.success() {
                    format!("taskkill 已返回成功，但进程仍在运行；Win32 兜底失败：{error:#}")
                } else {
                    format!(
                        "{}；Win32 兜底失败：{error:#}",
                        taskkill_output_text(&output.stdout, &output.stderr, output.status.code())
                    )
                };
                failures.push(format!("PID {}：{}", pid, detail));
            }
        }
    }
    Ok(failures)
}

/// 按镜像名兜底结束专用游戏进程，处理 PID 刚刷新导致的漏杀。
pub(crate) fn taskkill_images(image_names: &[&str]) -> Result<Vec<String>> {
    let mut failures = Vec::new();
    for image_name in image_names {
        let output = hidden_taskkill_command()
            .arg("/IM")
            .arg(image_name)
            .arg("/T")
            .arg("/F")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("结束进程镜像 {} 失败", image_name))?;

        if !process_image_exists(image_name) {
            continue;
        }

        let image_pids = process_pids_by_image(image_name);
        let mut terminate_errors = Vec::new();
        for pid in image_pids {
            match terminate_pid(pid) {
                Ok(()) => {}
                Err(error) if !process_exists(pid) => {
                    let _ = error;
                }
                Err(error) => terminate_errors.push(format!("PID {}：{error:#}", pid)),
            }
        }
        if !process_image_exists(image_name) {
            continue;
        }

        let detail = if output.status.success() {
            "taskkill 已返回成功，但进程仍在运行".to_string()
        } else {
            taskkill_output_text(&output.stdout, &output.stderr, output.status.code())
        };
        let fallback = if terminate_errors.is_empty() {
            String::new()
        } else {
            format!("；Win32 兜底失败：{}", terminate_errors.join("；"))
        };
        failures.push(format!("{}：{}", image_name, detail + &fallback));
    }
    Ok(failures)
}

fn terminate_pid(pid: u32) -> Result<()> {
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, false, pid)
            .with_context(|| format!("OpenProcess PID {} 失败", pid))?;
        let result = TerminateProcess(handle, 1)
            .with_context(|| format!("TerminateProcess PID {} 失败", pid));
        let _ = CloseHandle(handle);
        result
    }
}

fn process_exists(pid: u32) -> bool {
    let mut system = System::new_all();
    system.refresh_all();
    system.process(sysinfo::Pid::from_u32(pid)).is_some()
}

fn process_image_exists(image_name: &str) -> bool {
    let mut system = System::new_all();
    system.refresh_all();
    system
        .processes()
        .values()
        .any(|process| process_name_matches(&process.name().to_string_lossy(), image_name))
}

fn process_pids_by_image(image_name: &str) -> Vec<u32> {
    let mut system = System::new_all();
    system.refresh_all();
    system
        .processes()
        .iter()
        .filter_map(|(pid, process)| {
            process_name_matches(&process.name().to_string_lossy(), image_name)
                .then_some(pid.as_u32())
        })
        .collect()
}

fn process_name_matches(process_name: &str, image_name: &str) -> bool {
    let process_name = process_name.to_ascii_lowercase();
    let image_name = image_name.to_ascii_lowercase();
    if process_name == image_name {
        return true;
    }
    process_name.trim_end_matches(".exe") == image_name.trim_end_matches(".exe")
}

/// 选择最有用的 taskkill 诊断文本。
fn taskkill_output_text(stdout: &[u8], stderr: &[u8], code: Option<i32>) -> String {
    let stdout = String::from_utf8_lossy(stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        match code {
            Some(code) => format!("taskkill 退出码 {}", code),
            None => "taskkill 被系统终止".to_string(),
        }
    }
}

/// 从所选 Win64 目录推导启动所需的可执行文件路径。
pub(crate) fn launch_files(target_win64: &Path) -> LaunchFiles {
    LaunchFiles {
        server_dir: target_win64.join("BoundaryMetaServer-main"),
        node_exe: target_win64.join("nodejs").join("node.exe"),
        wrapper_exe: target_win64.join("ProjectReboundServerWrapper.exe"),
        game_exe: target_win64.join(GAME_EXE),
    }
}

/// 用于进程匹配的规范化小写路径。
pub(crate) fn path_match_key(path: &Path) -> String {
    path_text_match_key(
        &path
            .canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy(),
    )
}

/// 规范化从进程表拿到的路径/命令行文本，和 `path_match_key` 保持同一格式。
pub(crate) fn path_text_match_key(value: &str) -> String {
    let mut key = value
        .trim()
        .trim_matches('"')
        .replace('/', "\\")
        .to_lowercase();
    key = key.replace("\\\\?\\unc\\", "\\\\");
    key = key.replace("\\\\?\\", "");
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_text_match_key_strips_windows_extended_prefix() {
        assert_eq!(
            path_text_match_key(r#"\\?\D:\SteamLibrary\steamapps\common\Boundary"#),
            r#"d:\steamlibrary\steamapps\common\boundary"#
        );
        assert_eq!(
            path_text_match_key(r#"\\?\UNC\server\share\Boundary"#),
            r#"\\server\share\boundary"#
        );
    }

    #[test]
    fn process_name_matches_with_or_without_exe_suffix() {
        assert!(process_name_matches(
            "ProjectReboundServerWrapper",
            "ProjectReboundServerWrapper.exe"
        ));
        assert!(process_name_matches(
            "ProjectBoundarySteam-Win64-Shipping.exe",
            "ProjectBoundarySteam-Win64-Shipping"
        ));
    }
}
