//! Slint 按键事件到全局快捷键文本的转换。

pub(crate) fn hotkey_capture_is_escape(text: &str) -> bool {
    text.starts_with('\u{001b}')
}

/// 根据 Slint 的文本和修饰键字段构造快捷键字符串。
pub(crate) fn hotkey_from_capture(
    text: &str,
    control: bool,
    alt: bool,
    shift: bool,
    meta: bool,
) -> Option<String> {
    let key = captured_key_label(text)?;
    let mut parts = Vec::new();
    if control {
        parts.push("Ctrl".to_string());
    }
    if alt {
        parts.push("Alt".to_string());
    }
    if shift {
        parts.push("Shift".to_string());
    }
    if meta {
        parts.push("Win".to_string());
    }
    parts.push(key);
    Some(parts.join("+"))
}

/// 将 Slint 按键文本映射为 core::normalize_hotkey 接受的标签。
fn captured_key_label(text: &str) -> Option<String> {
    let mut chars = text.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    match ch {
        '\u{0010}' | '\u{0011}' | '\u{0012}' | '\u{0013}' | '\u{0017}' | '\u{0018}' => None,
        '\u{0009}' => Some("Tab".to_string()),
        '\u{000a}' => Some("Enter".to_string()),
        '\u{0020}' => Some("Space".to_string()),
        '\u{007f}' => Some("Delete".to_string()),
        '\u{F704}'..='\u{F71B}' => {
            let number = ch as u32 - '\u{F704}' as u32 + 1;
            Some(format!("F{number}"))
        }
        '\u{F727}' => Some("Insert".to_string()),
        '\u{F729}' => Some("Home".to_string()),
        '\u{F72B}' => Some("End".to_string()),
        '\u{F72C}' => Some("PageUp".to_string()),
        '\u{F72D}' => Some("PageDown".to_string()),
        value if value.is_ascii_alphabetic() => Some(value.to_ascii_uppercase().to_string()),
        value if value.is_ascii_digit() => Some(value.to_string()),
        _ => None,
    }
}
