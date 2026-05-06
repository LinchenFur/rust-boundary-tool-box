//! InstallerCore 启动、进程和端口运行时操作。

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
    path_text_match_key, runtime_image_pids, runtime_process_pids, summarize_runtime_processes,
    taskkill_images, taskkill_pids,
};
use super::util::hidden_command;
use super::*;

const RUNTIME_SHUTDOWN_IMAGES: &[&str] = &["ProjectReboundServerWrapper.exe", GAME_EXE];

impl InstallerCore {
    /// 按启动模式解析所需文件，并集中报告缺失路径。
    pub fn validate_launch_files(
        &self,
        target_win64: &Path,
        mode: LaunchMode,
    ) -> Result<LaunchFiles> {
        let files = launch_files(target_win64);
        let required = match mode {
            LaunchMode::Pvp => vec![&files.server_dir, &files.node_exe, &files.game_exe],
            LaunchMode::Pve => vec![
                &files.server_dir,
                &files.node_exe,
                &files.wrapper_exe,
                &files.game_exe,
            ],
        };
        let missing: Vec<String> = required
            .into_iter()
            .filter(|path| !path.exists())
            .map(|path| path.display().to_string())
            .collect();
        if !missing.is_empty() {
            bail!(
                "缺少 {} 启动所需文件：\n- {}",
                mode.display_name(),
                missing.join("\n- ")
            );
        }
        if mode.uses_login_server() {
            let login_missing: Vec<String> = [
                files.server_dir.join("index.js"),
                files.server_dir.join("package.json"),
                files.server_dir.join("node_modules").join("body-parser"),
                files.server_dir.join("node_modules").join("express"),
                files.server_dir.join("node_modules").join("protobufjs"),
            ]
            .into_iter()
            .filter(|path| !path.exists())
            .map(|path| path.display().to_string())
            .collect();
            if !login_missing.is_empty() {
                bail!(
                    "登录服务器依赖缺失，请重新安装后再启动：\n- {}",
                    login_missing.join("\n- ")
                );
            }
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
        let game_name = GAME_EXE.to_ascii_lowercase();

        let mut system = System::new_all();
        system.refresh_all();
        let mut snapshot = RuntimeSnapshot::default();
        for process in system.processes().values() {
            let exe_lower = process
                .exe()
                .map(|path| path_text_match_key(&path.to_string_lossy()))
                .unwrap_or_default();
            let name_lower = process.name().to_string_lossy().to_lowercase();
            let cmd = process
                .cmd()
                .iter()
                .map(|part| part.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ");
            let cmd_lower = path_text_match_key(&cmd);
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
                || (name_lower == game_name
                    && (launched_from_target || exe_lower.is_empty() || cmd_lower.is_empty()))
            {
                snapshot.game.push(item);
            } else if exe_lower == wrapper_exe
                || (name_lower == "projectreboundserverwrapper.exe" && launched_from_target)
            {
                snapshot.wrapper.push(item);
            } else if exe_lower == node_exe || (name_lower == "node.exe" && launched_from_target) {
                snapshot.server.push(item);
            }
        }
        Ok(snapshot)
    }

    /// 返回当前被占用的必要端口。
    pub fn collect_port_conflicts(&self) -> Result<Vec<PortConflict>> {
        collect_port_conflicts()
    }

    /// 为 UI 端口列表构造固定顺序的诊断行，并按当前目标目录标记预期进程。
    pub fn port_status_rows_for_target(
        &self,
        target_win64: Option<&Path>,
    ) -> Result<Vec<PortStatusRow>> {
        let mut conflicts = self.collect_port_conflicts()?;
        if let Some(target) = target_win64 {
            self.mark_expected_port_conflicts(target, &mut conflicts);
        }

        let mut rows = Vec::new();
        for (protocol, port) in MONITORED_PORTS.iter().copied() {
            let mut matches = conflicts
                .iter()
                .filter(|conflict| {
                    conflict.protocol.eq_ignore_ascii_case(protocol) && conflict.port == port
                })
                .cloned()
                .collect::<Vec<_>>();
            // 同一端口有多个 PID 时全部展示；异常占用排在前面，避免被目标进程视觉上遮住。
            matches.sort_by_key(|conflict| (conflict.expected, conflict.pid));
            if matches.is_empty() {
                rows.push(PortStatusRow {
                    protocol,
                    port,
                    conflict: None,
                });
            } else {
                for conflict in matches {
                    rows.push(PortStatusRow {
                        protocol,
                        port,
                        conflict: Some(conflict),
                    });
                }
            }
        }
        Ok(rows)
    }

    /// 根据当前目标目录的运行时快照给端口冲突打标；失败时保留端口结果继续展示。
    fn mark_expected_port_conflicts(&self, target_win64: &Path, conflicts: &mut [PortConflict]) {
        let Ok(snapshot) = self.collect_runtime_processes(target_win64) else {
            return;
        };
        let expected_pids = [&snapshot.game, &snapshot.wrapper, &snapshot.server]
            .into_iter()
            .flat_map(|group| group.iter().map(|process| process.pid))
            .collect::<Vec<_>>();
        for conflict in conflicts {
            conflict.expected = conflict.pid > 0 && expected_pids.contains(&conflict.pid);
        }
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

    /// 结束游戏、服务包装器和登录服务器进程。
    pub fn stop_runtime_processes(&self, target_win64: &Path) -> Result<String> {
        let initial = self.collect_runtime_processes(target_win64)?;
        let initial_image_pids = runtime_image_pids(RUNTIME_SHUTDOWN_IMAGES);
        if runtime_process_pids(&initial).is_empty() && initial_image_pids.is_empty() {
            self.log("关闭所有进程：未检测到需要关闭的相关进程。");
            return Ok("当前没有需要关闭的相关进程。".to_string());
        }
        self.log(format!(
            "关闭所有进程：初始检测到 {}{}",
            summarize_runtime_processes(&initial),
            image_pid_summary(&initial_image_pids)
        ));

        let mut killed_pids = Vec::new();
        let mut kill_failures = Vec::new();
        let mut empty_rounds = 0;
        for _ in 0..30 {
            let current = self.collect_runtime_processes(target_win64)?;
            let mut pids = runtime_process_pids(&current);
            append_missing_pids(&mut pids, &runtime_image_pids(RUNTIME_SHUTDOWN_IMAGES));
            if pids.is_empty() {
                empty_rounds += 1;
                if empty_rounds >= 4 {
                    self.log("关闭所有进程：已连续确认全部退出。");
                    return Ok(format!(
                        "已关闭相关进程：\n{}{}",
                        summarize_runtime_processes(&initial),
                        killed_pid_summary(&killed_pids)
                    ));
                }
                thread::sleep(Duration::from_millis(500));
                continue;
            }
            empty_rounds = 0;
            for pid in &pids {
                if !killed_pids.contains(pid) {
                    killed_pids.push(*pid);
                }
            }
            let failures = taskkill_pids(&pids)?;
            kill_failures.extend(failures);
            kill_failures.extend(taskkill_images(RUNTIME_SHUTDOWN_IMAGES)?);
            thread::sleep(Duration::from_millis(500));
        }

        let latest = self.collect_runtime_processes(target_win64)?;
        let latest_image_pids = runtime_image_pids(RUNTIME_SHUTDOWN_IMAGES);
        if runtime_process_pids(&latest).is_empty() && latest_image_pids.is_empty() {
            self.log("关闭所有进程：已确认全部退出。");
            return Ok(format!(
                "已关闭相关进程：\n{}{}",
                summarize_runtime_processes(&initial),
                killed_pid_summary(&killed_pids)
            ));
        }
        let failure_text = if kill_failures.is_empty() {
            String::new()
        } else {
            format!("\ntaskkill 失败详情：{}", kill_failures.join("；"))
        };
        bail!(
            "仍有相关进程在运行，可能有外部程序正在自动拉起游戏：{}{}{}",
            summarize_runtime_processes(&latest),
            image_pid_summary(&latest_image_pids),
            failure_text
        )
    }

    /// 安装前尽力关闭进程，并在结束后再次校验。
    pub(super) fn stop_runtime_processes_before_install(&self, target_win64: &Path) -> Result<()> {
        let snapshot = self.collect_runtime_processes(target_win64)?;
        let mut pids = runtime_process_pids(&snapshot);
        append_missing_pids(&mut pids, &runtime_image_pids(RUNTIME_SHUTDOWN_IMAGES));
        if pids.is_empty() {
            self.log("安装前检查：未检测到正在运行的相关进程。");
            return Ok(());
        }

        self.log(format!(
            "安装前关闭相关进程：{}",
            summarize_runtime_processes(&snapshot)
        ));
        let mut kill_failures = taskkill_pids(&pids)?;
        kill_failures.extend(taskkill_images(RUNTIME_SHUTDOWN_IMAGES)?);

        let mut empty_rounds = 0;
        for _ in 0..20 {
            thread::sleep(Duration::from_millis(250));
            let latest = self.collect_runtime_processes(target_win64)?;
            let mut latest_pids = runtime_process_pids(&latest);
            append_missing_pids(
                &mut latest_pids,
                &runtime_image_pids(RUNTIME_SHUTDOWN_IMAGES),
            );
            if latest_pids.is_empty() {
                empty_rounds += 1;
                if empty_rounds >= 3 {
                    if !kill_failures.is_empty() {
                        self.log(format!(
                            "安装前关闭相关进程：taskkill 返回失败但目标进程已退出：{}",
                            kill_failures.join("；")
                        ));
                    }
                    self.log("安装前关闭相关进程：已连续确认全部退出。");
                    return Ok(());
                }
                continue;
            }
            empty_rounds = 0;
            kill_failures.extend(taskkill_pids(&latest_pids)?);
            kill_failures.extend(taskkill_images(RUNTIME_SHUTDOWN_IMAGES)?);
        }

        let latest = self.collect_runtime_processes(target_win64)?;
        let latest_image_pids = runtime_image_pids(RUNTIME_SHUTDOWN_IMAGES);
        let failure_text = if kill_failures.is_empty() {
            String::new()
        } else {
            format!("\ntaskkill 失败详情：{}", kill_failures.join("；"))
        };
        bail!(
            "安装前仍有相关进程未退出，请手动关闭后重试：{}{}{}",
            summarize_runtime_processes(&latest),
            image_pid_summary(&latest_image_pids),
            failure_text
        )
    }

    /// 根据用户选择启动 PVP 或 PVE。
    pub fn launch(&self, target_win64: &Path, mode: LaunchMode) -> Result<String> {
        match mode {
            LaunchMode::Pvp => self.launch_pvp(target_win64),
            LaunchMode::Pve => self.launch_pve(target_win64),
        }
    }

    /// 启动登录服务器。
    fn launch_login_server(&self, files: &LaunchFiles) -> Result<()> {
        self.log(format!("启动登录服务器：{}", files.node_exe.display()));
        let mut child = hidden_command(&files.node_exe)
            .current_dir(&files.server_dir)
            .arg("index.js")
            .spawn()
            .context("启动登录服务器失败")?;
        thread::sleep(Duration::from_secs(1));
        if let Some(status) = child.try_wait().context("检查登录服务器进程状态失败")? {
            bail!("登录服务器启动后立即退出：{status}。请重新安装后再启动。");
        }
        Ok(())
    }

    /// 启动 PVP 流程：登录服务器和游戏，不启动 ProjectRebound 包装器。
    fn launch_pvp(&self, target_win64: &Path) -> Result<String> {
        validate_win64_path(target_win64)?;
        let files = self.validate_launch_files(target_win64, LaunchMode::Pvp)?;
        let cleaned = clean_engine_ini(self.logger.clone())?;

        self.launch_login_server(&files)?;
        thread::sleep(Duration::from_secs(5));
        self.log(format!("启动 PVP 游戏：{}", files.game_exe.display()));
        Command::new(&files.game_exe)
            .current_dir(target_win64)
            .arg(format!("-LogicServerURL={LOCAL_LOGIC_SERVER_URL}"))
            .spawn()
            .context("启动 PVP 游戏失败")?;

        let mut notes = vec!["PVP 启动完成。".to_string()];
        if let Some(path) = cleaned {
            notes.push(format!("并已清理冲突配置：{}", path.display()));
        }
        Ok(notes.join("\n"))
    }

    /// 启动登录服务器、ProjectRebound 包装器和 PVE 游戏。
    fn launch_pve(&self, target_win64: &Path) -> Result<String> {
        validate_win64_path(target_win64)?;
        let files = self.validate_launch_files(target_win64, LaunchMode::Pve)?;
        let cleaned = clean_engine_ini(self.logger.clone())?;

        self.launch_login_server(&files)?;
        thread::sleep(Duration::from_secs(5));

        self.log(format!(
            "启动 PVE 服务包装器：{}",
            files.wrapper_exe.display()
        ));
        hidden_command(&files.wrapper_exe)
            .current_dir(target_win64)
            .spawn()
            .context("启动 PVE 服务包装器失败")?;
        thread::sleep(Duration::from_secs(2));

        self.log(format!("启动 PVE 游戏：{}", files.game_exe.display()));
        Command::new(&files.game_exe)
            .current_dir(target_win64)
            .arg(format!("-LogicServerURL={LOCAL_LOGIC_SERVER_URL}"))
            .spawn()
            .context("启动 PVE 游戏失败")?;

        let mut notes = vec!["PVE 启动完成。".to_string()];
        if let Some(path) = cleaned {
            notes.push(format!("并已清理冲突配置：{}", path.display()));
        }
        Ok(notes.join("\n"))
    }
}

fn killed_pid_summary(pids: &[u32]) -> String {
    if pids.is_empty() {
        String::new()
    } else {
        format!(
            "\n已请求结束 PID：{}",
            pids.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}

fn image_pid_summary(pids: &[u32]) -> String {
    if pids.is_empty() {
        String::new()
    } else {
        format!(
            "；专用镜像 PID {}",
            pids.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}

fn append_missing_pids(target: &mut Vec<u32>, extra: &[u32]) {
    for pid in extra {
        if *pid > 0 && !target.contains(pid) {
            target.push(*pid);
        }
    }
}
