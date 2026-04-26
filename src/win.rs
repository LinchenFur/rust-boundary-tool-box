//! Windows-specific helpers for global hotkeys and topmost game-window control.
//!
//! The main application starts a hidden watcher process after launching the
//! game. That watcher finds the game window, optionally keeps it topmost, and
//! listens for a global hotkey to toggle the behavior.

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

/// Normalized global hotkey definition ready for RegisterHotKey.
#[derive(Debug, Clone)]
pub struct HotkeyDefinition {
    pub normalized: String,
    pub modifiers: HOT_KEY_MODIFIERS,
    pub vk_code: u32,
}

/// Parses user-facing hotkey text such as Ctrl+Alt+F10.
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
        // Modifiers can appear in any order but are normalized later.
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
            // Single ASCII letters/digits map directly to virtual-key codes.
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

        // Function keys use the contiguous VK_F1..VK_F24 range.
        if lower.starts_with('f') && lower[1..].chars().all(|ch| ch.is_ascii_digit()) {
            let number = lower[1..].parse::<u32>()?;
            if (1..=24).contains(&number) {
                main_label = Some(format!("F{}", number));
                vk_code = Some(0x70 + number - 1);
                continue;
            }
        }

        // Named keys are limited to the set useful for a simple toggle shortcut.
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
    // Store modifiers in a stable order so the UI does not bounce between
    // equivalent strings like Alt+Ctrl+F10 and Ctrl+Alt+F10.
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

/// Top-level native window candidate found during enumeration.
#[derive(Debug, Clone)]
struct WindowCandidate {
    hwnd: HWND,
    title: String,
    pid: u32,
}

/// Watches one game process and toggles topmost state through a global hotkey.
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
        // The watcher is a hidden process with its own message queue. Polling
        // WM_HOTKEY keeps the logic simple and avoids a visible window.
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
            // First try the exact game PID; fallback covers launchers that spawn
            // a child window under another process.
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

/// Drains WM_HOTKEY messages from the watcher thread queue.
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

/// Finds the first visible window owned by a process ID.
fn find_window_by_pid(process_id: u32) -> Option<HWND> {
    enum_windows()
        .into_iter()
        .find(|window| window.pid == process_id)
        .map(|window| window.hwnd)
}

/// Fallback title scan for cases where the visible game window has a child PID.
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

/// Enumerates visible top-level windows with titles and process IDs.
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

/// Restores/focuses a window and applies topmost; can optionally release it.
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

/// Changes topmost state without stealing focus.
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
