//! 远程社区服列表获取、解析和行模型转换。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::ServerRow;

// 远程社区服列表接口。请求逻辑刻意保持很小，并用 TcpStream 实现，
// 这样 UI 层不需要额外的异步 HTTP 运行时。
const SERVER_LIST_HOST: &str = "ax48735790k.vicp.fun";
const SERVER_LIST_PORT: u16 = 3000;
const SERVER_LIST_PATH: &str = "/servers";

/// 社区服 JSON 接口返回的一行数据。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct RemoteServer {
    name: String,
    region: String,
    mode: String,
    map: String,
    port: u16,
    player_count: u32,
    server_state: String,
    ip: String,
    last_heartbeat: i64,
}

/// 拉取并解析远程社区服列表。
pub(crate) fn fetch_servers() -> Result<Vec<RemoteServer>> {
    let body = http_get_json(SERVER_LIST_HOST, SERVER_LIST_PORT, SERVER_LIST_PATH)
        .context("请求服务器列表接口失败")?;
    let servers =
        serde_json::from_str::<Vec<RemoteServer>>(&body).context("解析服务器列表 JSON 失败")?;
    Ok(servers)
}

fn http_get_json(host: &str, port: u16, path: &str) -> Result<String> {
    let mut stream =
        TcpStream::connect((host, port)).with_context(|| format!("连接 {host}:{port} 失败"))?;
    stream.set_read_timeout(Some(Duration::from_secs(12)))?;
    stream.set_write_timeout(Some(Duration::from_secs(8)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUser-Agent: boundary-toolbox/1.2\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    )?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let header_end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("服务器响应缺少 HTTP 头")?;
    let header = String::from_utf8_lossy(&raw[..header_end]);
    let mut lines = header.lines();
    let status_line = lines.next().unwrap_or_default();
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);
    if !(200..300).contains(&status_code) {
        bail!("服务器返回 HTTP {status_code}");
    }

    // 服务器可能返回固定 Content-Length，也可能返回 transfer-encoding: chunked。
    // 两种都解码，保证 UI 列表刷新足够稳。
    let is_chunked = lines.any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("transfer-encoding:") && lower.contains("chunked")
    });
    let body_bytes = &raw[header_end + 4..];
    let decoded = if is_chunked {
        decode_chunked_body(body_bytes)?
    } else {
        body_bytes.to_vec()
    };
    String::from_utf8(decoded).context("服务器列表响应不是 UTF-8")
}

/// 解码较小的 HTTP chunked 响应正文。
fn decode_chunked_body(mut body: &[u8]) -> Result<Vec<u8>> {
    let mut decoded = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|window| window == b"\r\n")
            .context("chunked 响应格式错误")?;
        let size_text = String::from_utf8_lossy(&body[..line_end]);
        let size_hex = size_text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_hex, 16)
            .with_context(|| format!("无效 chunk 大小：{size_hex}"))?;
        body = &body[line_end + 2..];
        if size == 0 {
            break;
        }
        if body.len() < size + 2 {
            bail!("chunked 响应正文不完整");
        }
        decoded.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
    Ok(decoded)
}

/// 将远程服务器 JSON 转换为紧凑 UI 行。
pub(crate) fn server_to_row(server: RemoteServer) -> ServerRow {
    let state = normalize_server_state(&server.server_state);
    let active = state != "状态未知";
    ServerRow {
        name: shorten_text(&server.name, 44).into(),
        address: format!("{}:{}", empty_as_dash(&server.ip), server.port).into(),
        meta: format!(
            "{} / {} / {} / 更新时间 {}",
            empty_as_dash(&server.region),
            empty_as_dash(&server.mode),
            empty_as_dash(&server.map),
            format_heartbeat(server.last_heartbeat)
        )
        .into(),
        state: state.into(),
        players: format!("{} 人", server.player_count).into(),
        active,
    }
}

/// 加载中或错误后使用的占位行。
pub(crate) fn server_placeholder_row(title: &str, detail: &str) -> ServerRow {
    ServerRow {
        name: title.into(),
        address: detail.into(),
        meta: "服务器列表".into(),
        state: "WAIT".into(),
        players: "--".into(),
        active: false,
    }
}

/// 规范化空白或无效的服务器状态字符串。
fn normalize_server_state(state: &str) -> String {
    match state.trim() {
        "" | "InvalidState" => "状态未知".to_string(),
        value => value.to_string(),
    }
}

/// 将远程接口中的空字段显示为 `-`。
fn empty_as_dash(value: &str) -> &str {
    if value.trim().is_empty() {
        "-"
    } else {
        value.trim()
    }
}

/// 缩短过长服务器名，避免撑破列表行。
fn shorten_text(value: &str, max_chars: usize) -> String {
    let mut text = value.trim().to_string();
    if text.chars().count() <= max_chars {
        return text;
    }
    text = text.chars().take(max_chars.saturating_sub(3)).collect();
    text.push_str("...");
    text
}

/// 以本地时间格式化服务器最后心跳时间戳。
fn format_heartbeat(timestamp_ms: i64) -> String {
    if timestamp_ms <= 0 {
        return "-".to_string();
    }
    chrono::DateTime::from_timestamp_millis(timestamp_ms)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "-".to_string())
}
