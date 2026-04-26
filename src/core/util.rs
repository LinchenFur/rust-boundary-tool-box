//! 通用时间、快捷键、目录和隐藏进程工具。

use super::*;

/// 供 UI 和日志使用的本地时间戳。
pub fn now_text() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 写入元数据的紧凑 ISO 风格时间戳。
pub fn iso_now() -> String {
    Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

/// 当前置顶目标固定为游戏窗口。
pub fn normalize_topmost_mode(_value: &str) -> String {
    DEFAULT_TOPMOST_MODE.to_string()
}

/// 解析置顶开关持久化字符串中的真假值。
pub fn normalize_keep_topmost(value: impl ToString) -> bool {
    !matches!(
        value.to_string().trim().to_lowercase().as_str(),
        "" | "0" | "false" | "no" | "off"
    )
}

/// 校验并规范化用户输入或捕获到的快捷键。
pub fn normalize_hotkey(value: impl AsRef<str>) -> Result<String> {
    Ok(parse_hotkey_text(value.as_ref())?.normalized)
}

/// 处理隐藏置顶守护子进程的命令行模式。
pub fn watch_mode_from_args(args: &[String]) -> Option<Result<i32>> {
    let pid = cli_value(args, "--watch-pid")?.parse::<u32>().ok()?;
    let hotkey = cli_value(args, "--hotkey").unwrap_or_else(|| DEFAULT_TOPMOST_HOTKEY.to_string());
    let keep_topmost = args.iter().any(|item| item == "--keep-topmost");
    Some(watch_window_by_pid(pid, keep_topmost, &hotkey))
}

/// 读取命令行标志后面的值。
fn cli_value(args: &[String], flag: &str) -> Option<String> {
    let index = args.iter().position(|item| item == flag)?;
    args.get(index + 1).cloned()
}

/// 将安装器数据放在可执行文件旁边或 installer_tool 目录下。
pub(crate) fn resolve_installer_home(runtime_dir: &Path) -> PathBuf {
    if runtime_dir
        .file_name()
        .map(|name| {
            name.to_string_lossy()
                .eq_ignore_ascii_case(INSTALLER_FOLDER_NAME)
        })
        .unwrap_or(false)
    {
        runtime_dir.to_path_buf()
    } else {
        runtime_dir.join(INSTALLER_FOLDER_NAME)
    }
}

/// 创建目录树，并把文件系统错误转成 anyhow。
pub(crate) fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    Ok(())
}

/// 创建无控制台窗口且不继承标准输入输出的 Windows 命令。
pub(crate) fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// 单独封装 taskkill，便于统一参数和后续测试。
pub(crate) fn hidden_taskkill_command() -> Command {
    hidden_command("taskkill")
}
