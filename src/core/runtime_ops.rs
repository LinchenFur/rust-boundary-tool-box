//! InstallerCore 启动、进程和端口运行时操作。

use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use sysinfo::System;

use super::cleanup::clean_engine_ini;
use super::pathing::validate_win64_path;
use super::process::{
    collect_port_conflicts, format_port_conflicts, kill_pids, launch_files, path_match_key,
    runtime_process_pids, summarize_runtime_processes, taskkill_pids,
};
use super::util::hidden_command;
use super::*;

impl InstallerCore {
    /// 解析启动所需全部文件，并集中报告缺失路径。
    pub fn validate_launch_files(&self, target_win64: &Path) -> Result<LaunchFiles> {
        let files = launch_files(target_win64);
        let missing: Vec<String> = [
            &files.server_dir,
            &files.node_exe,
            &files.wrapper_exe,
            &files.game_exe,
        ]
        .iter()
        .filter(|path| !path.exists())
        .map(|path| path.display().to_string())
        .collect();
        if !missing.is_empty() {
            bail!("缺少启动所需文件：\n- {}", missing.join("\n- "));
        }
        Ok(files)
    }

    /// 仅收集属于所选目标目录的 Boundary 运行时进程。
    ///
    /// 匹配范围刻意限定为可执行路径或命令行包含目标目录，避免误杀其它目录下
    /// 运行 index.js 的无关 node.exe 服务。
    pub fn collect_runtime_processes(&self, target_win64: &Path) -> Result<RuntimeSnapshot> {
        let files = launch_files(target_win64);
        let game_exe = path_match_key(&files.game_exe);
        let wrapper_exe = path_match_key(&files.wrapper_exe);
        let node_exe = path_match_key(&files.node_exe);
        let target_dir = path_match_key(target_win64);
        let watcher_exe = path_match_key(&env::current_exe()?);

        let mut system = System::new_all();
        system.refresh_all();
        let mut snapshot = RuntimeSnapshot::default();
        for process in system.processes().values() {
            let exe_lower = process
                .exe()
                .map(|path| path.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let name_lower = process.name().to_string_lossy().to_lowercase();
            let cmd = process
                .cmd()
                .iter()
                .map(|part| part.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ");
            let cmd_lower = cmd.to_lowercase();
            let item = RuntimeProcess {
                pid: process.pid().as_u32(),
                name: process.name().to_string_lossy().into_owned(),
                exe: process
                    .exe()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
                cmd,
            };
            let launched_from_target =
                exe_lower.contains(&target_dir) || cmd_lower.contains(&target_dir);

            // 优先使用精确可执行路径；只有命令行能证明进程属于当前安装时，
            // 才退回到进程名匹配。
            if exe_lower == game_exe
                || (name_lower == GAME_EXE.to_ascii_lowercase() && launched_from_target)
            {
                snapshot.game.push(item);
            } else if exe_lower == wrapper_exe
                || (name_lower == "projectreboundserverwrapper.exe" && launched_from_target)
            {
                snapshot.wrapper.push(item);
            } else if exe_lower == node_exe || (name_lower == "node.exe" && launched_from_target) {
                snapshot.server.push(item);
            } else if exe_lower == watcher_exe && cmd_lower.contains("--watch-pid") {
                snapshot.watcher.push(item);
            }
        }
        Ok(snapshot)
    }

    /// 返回当前被占用的必要端口。
    pub fn collect_port_conflicts(&self) -> Result<Vec<PortConflict>> {
        collect_port_conflicts()
    }

    /// 为 UI 端口列表构造固定顺序的诊断行。
    pub fn port_status_rows(&self) -> Result<Vec<PortStatusRow>> {
        let conflicts = self.collect_port_conflicts()?;
        let mut map = HashMap::new();
        for conflict in conflicts {
            map.insert((conflict.protocol.to_uppercase(), conflict.port), conflict);
        }
        let rows = MONITORED_PORTS
            .iter()
            .map(|(protocol, port)| PortStatusRow {
                protocol,
                port: *port,
                conflict: map.get(&(protocol.to_string(), *port)).cloned(),
            })
            .collect();
        Ok(rows)
    }

    /// 结束占用必要端口的进程。
    pub fn stop_port_conflict_processes(&self, conflicts: &[PortConflict]) -> Result<String> {
        let mut pids = Vec::new();
        for conflict in conflicts {
            if conflict.pid > 0 && !pids.contains(&conflict.pid) {
                pids.push(conflict.pid);
            }
        }
        if pids.is_empty() {
            return Ok("未找到可关闭的占用进程。".to_string());
        }
        kill_pids(&pids)?;
        self.log(format!(
            "端口占用清理：已请求结束 PID {}",
            pids.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ));
        Ok(format!(
            "已关闭以下端口占用进程：\n{}",
            format_port_conflicts(conflicts)
        ))
    }

    /// 结束游戏、服务包装器、登录服务器和置顶守护进程。
    pub fn stop_runtime_processes(&self, target_win64: &Path) -> Result<String> {
        let snapshot = self.collect_runtime_processes(target_win64)?;
        let mut pids = Vec::new();
        for group in [
            &snapshot.watcher,
            &snapshot.game,
            &snapshot.wrapper,
            &snapshot.server,
        ] {
            for process in group.iter() {
                if process.pid > 0 && !pids.contains(&process.pid) {
                    pids.push(process.pid);
                }
            }
        }
        if pids.is_empty() {
            self.log("关闭所有进程：未检测到需要关闭的相关进程。");
            return Ok("当前没有需要关闭的相关进程。".to_string());
        }
        kill_pids(&pids)?;
        self.log(format!(
            "关闭所有进程：已请求结束 PID {}",
            pids.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ));
        Ok(format!(
            "已关闭相关进程：\n{}",
            summarize_runtime_processes(&snapshot)
        ))
    }

    /// 安装前尽力关闭进程，并在结束后再次校验。
    pub(super) fn stop_runtime_processes_before_install(&self, target_win64: &Path) -> Result<()> {
        let snapshot = self.collect_runtime_processes(target_win64)?;
        let pids = runtime_process_pids(&snapshot);
        if pids.is_empty() {
            self.log("安装前检查：未检测到正在运行的相关进程。");
            return Ok(());
        }

        self.log(format!(
            "安装前关闭相关进程：{}",
            summarize_runtime_processes(&snapshot)
        ));
        let kill_failures = taskkill_pids(&pids)?;

        for _ in 0..20 {
            thread::sleep(Duration::from_millis(250));
            let latest = self.collect_runtime_processes(target_win64)?;
            if runtime_process_pids(&latest).is_empty() {
                if !kill_failures.is_empty() {
                    self.log(format!(
                        "安装前关闭相关进程：taskkill 返回失败但目标进程已退出：{}",
                        kill_failures.join("；")
                    ));
                }
                self.log("安装前关闭相关进程：已全部退出。");
                return Ok(());
            }
        }

        let latest = self.collect_runtime_processes(target_win64)?;
        let failure_text = if kill_failures.is_empty() {
            String::new()
        } else {
            format!("\ntaskkill 失败详情：{}", kill_failures.join("；"))
        };
        bail!(
            "安装前仍有相关进程未退出，请手动关闭后重试：{}{}",
            summarize_runtime_processes(&latest),
            failure_text
        )
    }

    /// 启动登录服务器、ProjectRebound 包装器、游戏和置顶守护。
    pub fn launch(&self, target_win64: &Path, keep_topmost: bool, hotkey: &str) -> Result<String> {
        validate_win64_path(target_win64)?;
        let files = self.validate_launch_files(target_win64)?;
        let cleaned = clean_engine_ini(self.logger.clone())?;
        let topmost = self.write_topmost_config(target_win64, keep_topmost, hotkey)?;

        self.log(format!("启动登录服务器：{}", files.node_exe.display()));
        hidden_command(&files.node_exe)
            .current_dir(&files.server_dir)
            .arg("index.js")
            .spawn()
            .context("启动登录服务器失败")?;
        thread::sleep(Duration::from_secs(5));

        self.log(format!("启动服务包装器：{}", files.wrapper_exe.display()));
        hidden_command(&files.wrapper_exe)
            .current_dir(target_win64)
            .spawn()
            .context("启动服务包装器失败")?;
        thread::sleep(Duration::from_secs(2));

        self.log(format!("启动游戏：{}", files.game_exe.display()));
        let game_process = Command::new(&files.game_exe)
            .current_dir(target_win64)
            .arg("-LogicServerURL=http://127.0.0.1:8000")
            .spawn()
            .context("启动游戏失败")?;

        self.log("Rust 置顶守护：目标固定为游戏窗口。");
        let mut watcher = hidden_command(env::current_exe()?);
        watcher
            .arg("--watch-pid")
            .arg(game_process.id().to_string())
            .arg("--hotkey")
            .arg(topmost.hotkey.clone());
        if topmost.keep_topmost {
            watcher.arg("--keep-topmost");
        }
        watcher.spawn().context("启动置顶守护失败")?;

        let mut notes = vec![
            "启动完成。".to_string(),
            format!("窗口置顶目标：{}", TOPMOST_GAME_LABEL),
            if topmost.keep_topmost {
                "持续置顶：默认已开启，按开关键可关闭或重新开启".to_string()
            } else {
                "持续置顶：默认已关闭，按开关键可开启或再次关闭".to_string()
            },
            format!("持续置顶开关键：{}", topmost.hotkey),
            "原版批处理仍保留为 startgame.bat，未被修改参与该功能。".to_string(),
        ];
        if let Some(path) = cleaned {
            notes.push(format!("并已清理冲突配置：{}", path.display()));
        }
        Ok(notes.join("\n"))
    }
}
