//! 面向 Windows 的全局快捷键和游戏窗口置顶辅助逻辑。
//!
//! 主应用启动游戏后会拉起一个隐藏守护进程。守护进程负责寻找游戏窗口，
//! 按配置保持置顶，并监听全局快捷键来切换该行为。

use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    HWND_NOTOPMOST, HWND_TOPMOST, IsWindow, IsWindowVisible, MSG, PM_REMOVE, PeekMessageW,
    SW_RESTORE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetForegroundWindow, SetWindowPos,
    ShowWindow, TranslateMessage, WM_HOTKEY,
};
use windows::core::BOOL;

use crate::core::{
    DEFAULT_TOPMOST_HOTKEY, TOPMOST_WATCH_LOST_TIMEOUT_SECONDS, TOPMOST_WATCH_START_TIMEOUT_SECONDS,
};

const HOTKEY_ID: i32 = 0x5045_5645;

/// 已规范化、可直接传给 RegisterHotKey 的全局快捷键定义。
#[derive(Debug, Clone)]
pub struct HotkeyDefinition {
    pub normalized: String,
    pub modifiers: HOT_KEY_MODIFIERS,
    pub vk_code: u32,
}

/// 解析用户可读的快捷键文本，例如 Ctrl+Alt+F10。
pub fn parse_hotkey_text(value: &str) -> Result<HotkeyDefinition> {
    let raw = if value.trim().is_empty() {
        DEFAULT_TOPMOST_HOTKEY
    } else {
        value.trim()
    };
    let tokens = raw
        .split('+')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        bail!("快捷键不能为空。");
    }

    let mut modifiers = HOT_KEY_MODIFIERS(0);
    let mut seen: Vec<String> = Vec::new();
    let mut main_label: Option<String> = None;
    let mut vk_code: Option<u32> = None;

    for token in tokens {
        let lower = token.to_lowercase();
        // 修饰键可以任意排序，稍后会统一规范化。
        let modifier = match lower.as_str() {
            "ctrl" | "control" => Some(("Ctrl", MOD_CONTROL.0)),
            "alt" => Some(("Alt", MOD_ALT.0)),
            "shift" => Some(("Shift", MOD_SHIFT.0)),
            "win" | "windows" => Some(("Win", MOD_WIN.0)),
            _ => None,
        };
        if let Some((label, flag)) = modifier {
            if seen.iter().any(|item| item == label) {
                bail!("快捷键包含重复修饰键：{}", label);
            }
            seen.push(label.to_string());
            modifiers |= HOT_KEY_MODIFIERS(flag);
            continue;
        }

        if main_label.is_some() {
            bail!("快捷键只能包含一个主键，例如 Ctrl+Alt+F10。");
        }

        if token.len() == 1 {
            // 单个 ASCII 字母或数字可直接映射到虚拟键码。
            let ch = token.chars().next().unwrap();
            if ch.is_ascii_alphabetic() {
                main_label = Some(ch.to_ascii_uppercase().to_string());
                vk_code = Some(ch.to_ascii_uppercase() as u32);
                continue;
            }
            if ch.is_ascii_digit() {
                main_label = Some(ch.to_string());
                vk_code = Some(ch as u32);
                continue;
            }
        }

        // 功能键使用连续的 VK_F1..VK_F24 范围。
        if lower.starts_with('f') && lower[1..].chars().all(|ch| ch.is_ascii_digit()) {
            let number = lower[1..].parse::<u32>()?;
            if (1..=24).contains(&number) {
                main_label = Some(format!("F{}", number));
                vk_code = Some(0x70 + number - 1);
                continue;
            }
        }

        // 命名按键限制在简单切换快捷键常用的集合内。
        let special = match lower.as_str() {
            "space" => Some(("Space", 0x20)),
            "tab" => Some(("Tab", 0x09)),
            "esc" | "escape" => Some(("Esc", 0x1B)),
            "enter" | "return" => Some(("Enter", 0x0D)),
            "insert" | "ins" => Some(("Insert", 0x2D)),
            "delete" | "del" => Some(("Delete", 0x2E)),
            "home" => Some(("Home", 0x24)),
            "end" => Some(("End", 0x23)),
            "pageup" | "pgup" => Some(("PageUp", 0x21)),
            "pagedown" | "pgdn" => Some(("PageDown", 0x22)),
            _ => None,
        };
        if let Some((label, vk)) = special {
            main_label = Some(label.to_string());
            vk_code = Some(vk);
            continue;
        }

        bail!("快捷键格式无效，请使用类似 Ctrl+Alt+F10、Ctrl+Shift+G、F9 这样的格式。");
    }

    let Some(main_label) = main_label else {
        bail!("快捷键缺少主键，请使用类似 Ctrl+Alt+F10 的格式。");
    };
    let Some(vk_code) = vk_code else {
        bail!("快捷键缺少主键，请使用类似 Ctrl+Alt+F10 的格式。");
    };
    // 以稳定顺序存储修饰键，避免 UI 在 Alt+Ctrl+F10 和 Ctrl+Alt+F10
    // 这类等价字符串之间来回变化。
    let ordered = ["Ctrl", "Alt", "Shift", "Win"]
        .into_iter()
        .filter(|label| seen.contains(&label.to_string()))
        .map(ToOwned::to_owned)
        .chain(std::iter::once(main_label))
        .collect::<Vec<_>>();
    Ok(HotkeyDefinition {
        normalized: ordered.join("+"),
        modifiers,
        vk_code,
    })
}

/// 枚举时找到的顶层原生窗口候选项。
#[derive(Debug, Clone)]
struct WindowCandidate {
    hwnd: HWND,
    title: String,
    pid: u32,
}

/// 监控一个游戏进程，并通过全局快捷键切换置顶状态。
pub fn watch_window_by_pid(process_id: u32, keep_topmost: bool, hotkey_text: &str) -> Result<i32> {
    let hotkey = parse_hotkey_text(hotkey_text)?;
    let mut hotkey_registered = false;
    unsafe {
        if RegisterHotKey(None, HOTKEY_ID, hotkey.modifiers, hotkey.vk_code).is_ok() {
            hotkey_registered = true;
        }
    }

    let mut hwnd: Option<HWND> = None;
    let mut topmost_enabled = keep_topmost;
    let start = Instant::now();
    let mut last_seen_window = Instant::now();
    let mut last_toggle_state: Option<bool> = None;

    loop {
        // 守护进程是隐藏进程，拥有自己的消息队列。轮询 WM_HOTKEY
        // 可以让逻辑保持简单，同时不需要可见窗口。
        while consume_hotkey_messages()? {
            topmost_enabled = !topmost_enabled;
            if let Some(current_hwnd) = hwnd {
                if topmost_enabled {
                    force_window_topmost(current_hwnd, true)?;
                } else {
                    set_window_topmost_enabled(current_hwnd, false)?;
                }
            }
        }

        let window_exists = hwnd.is_some_and(|handle| unsafe { IsWindow(Some(handle)).as_bool() });
        if !window_exists {
            // 优先按精确游戏 PID 查找；兜底逻辑覆盖启动器把窗口放到子进程的情况。
            hwnd = find_window_by_pid(process_id).or_else(find_boundary_window_fallback);
            if hwnd.is_some() {
                last_seen_window = Instant::now();
            }
        }

        if let Some(current_hwnd) = hwnd {
            last_seen_window = Instant::now();
            if last_toggle_state != Some(topmost_enabled) {
                if topmost_enabled {
                    force_window_topmost(current_hwnd, true)?;
                } else {
                    set_window_topmost_enabled(current_hwnd, false)?;
                }
                last_toggle_state = Some(topmost_enabled);
            } else if topmost_enabled {
                force_window_topmost(current_hwnd, true)?;
            }
        } else {
            let start_timeout = Duration::from_secs(TOPMOST_WATCH_START_TIMEOUT_SECONDS);
            if start.elapsed() > start_timeout {
                break;
            }
        }

        if hwnd.is_some()
            && last_seen_window.elapsed() > Duration::from_secs(TOPMOST_WATCH_LOST_TIMEOUT_SECONDS)
        {
            break;
        }
        thread::sleep(Duration::from_millis(350));
    }

    if hotkey_registered {
        unsafe {
            let _ = UnregisterHotKey(None, HOTKEY_ID);
        }
    }
    Ok(0)
}

/// 从守护线程消息队列中取出 WM_HOTKEY 消息。
fn consume_hotkey_messages() -> Result<bool> {
    let mut consumed = false;
    unsafe {
        let mut message = MSG::default();
        while PeekMessageW(&mut message, None, WM_HOTKEY, WM_HOTKEY, PM_REMOVE).as_bool() {
            if message.message == WM_HOTKEY && message.wParam == WPARAM(HOTKEY_ID as usize) {
                consumed = true;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(consumed)
}

/// 查找指定进程 ID 拥有的第一个可见窗口。
fn find_window_by_pid(process_id: u32) -> Option<HWND> {
    enum_windows()
        .into_iter()
        .find(|window| window.pid == process_id)
        .map(|window| window.hwnd)
}

/// 当可见游戏窗口属于子 PID 时，通过标题扫描兜底。
fn find_boundary_window_fallback() -> Option<HWND> {
    enum_windows().into_iter().find_map(|window| {
        let title = window.title.to_lowercase();
        if title.contains("boundary") || title.contains("projectboundary") {
            Some(window.hwnd)
        } else {
            None
        }
    })
}

/// 枚举带标题和进程 ID 的可见顶层窗口。
fn enum_windows() -> Vec<WindowCandidate> {
    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let windows = unsafe { &mut *(lparam.0 as *mut Vec<WindowCandidate>) };
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            return true.into();
        }
        let length = unsafe { GetWindowTextLengthW(hwnd) };
        if length <= 0 {
            return true.into();
        }
        let mut buffer = vec![0_u16; length as usize + 1];
        let written = unsafe { GetWindowTextW(hwnd, &mut buffer) };
        if written <= 0 {
            return true.into();
        }
        let title = String::from_utf16_lossy(&buffer[..written as usize])
            .trim()
            .to_string();
        if title.is_empty() {
            return true.into();
        }
        let mut pid = 0_u32;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        windows.push(WindowCandidate { hwnd, title, pid });
        true.into()
    }

    let mut windows = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(callback), LPARAM(&mut windows as *mut _ as isize));
    }
    windows
}

/// 恢复并聚焦窗口后应用置顶；也可以按需解除置顶。
fn force_window_topmost(hwnd: HWND, keep_topmost: bool) -> Result<()> {
    unsafe {
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = SetForegroundWindow(hwnd);
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
        )?;
        if !keep_topmost {
            SetWindowPos(
                hwnd,
                Some(HWND_NOTOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
            )?;
        }
    }
    Ok(())
}

/// 在不抢占焦点的情况下修改置顶状态。
fn set_window_topmost_enabled(hwnd: HWND, enabled: bool) -> Result<()> {
    unsafe {
        SetWindowPos(
            hwnd,
            if enabled {
                Some(HWND_TOPMOST)
            } else {
                Some(HWND_NOTOPMOST)
            },
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
        )?;
    }
    Ok(())
}
