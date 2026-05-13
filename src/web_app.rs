//! WebView-backed application controller.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, RwLock};
use std::thread;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::core::{
    APP_VERSION, DEFAULT_GITHUB_PROXY_PREFIX, InstallCancelToken, InstallProgress, InstallerCore,
    LaunchMode, PortStatusRow, RuntimeProcess, RuntimeSnapshot, is_running_as_administrator,
};
use crate::webview_host::{MessageHandler, WebView, WebViewProxy};

const INDEX_HTML: &str = include_str!("../ui-web/index.html");
const LGGC_CSS: &str = include_str!("../ui-web/vendor/lggc.css");
const APP_CSS: &str = include_str!("../ui-web/styles.css");
const APP_JS: &str = include_str!("../ui-web/app.js");

struct WebAppState {
    core: Arc<InstallerCore>,
    target: Arc<RwLock<Option<PathBuf>>>,
    proxy_sink: Arc<Mutex<Option<WebViewProxy>>>,
    install_cancel: Arc<Mutex<Option<InstallCancelToken>>>,
}

pub(crate) fn run() -> Result<()> {
    let proxy_sink = Arc::new(Mutex::new(None::<WebViewProxy>));
    let logger_proxy = proxy_sink.clone();
    let logger = Arc::new(move |line: String| {
        if let Some(proxy) = logger_proxy.lock().ok().and_then(|guard| guard.clone()) {
            emit(&proxy, json!({ "type": "log", "line": line }));
        }
    });

    let core = Arc::new(InstallerCore::new(logger)?);
    core.set_github_proxy_prefix(DEFAULT_GITHUB_PROXY_PREFIX);
    let state = Arc::new(WebAppState {
        core,
        target: Arc::new(RwLock::new(None)),
        proxy_sink,
        install_cancel: Arc::new(Mutex::new(None)),
    });

    let handler_state = state.clone();
    let handler: MessageHandler = Rc::new(RefCell::new(move |value: Value, webview: WebView| {
        if let Err(error) = handle_message(value, webview.clone(), handler_state.clone()) {
            emit(
                &webview.proxy(),
                json!({ "type": "dialog", "title": "操作失败", "message": error.to_string() }),
            );
        }
    }));

    let webview = WebView::create(cfg!(debug_assertions), handler)
        .map_err(|error| anyhow!("创建 WebView2 窗口失败：{error}"))?;
    *state
        .proxy_sink
        .lock()
        .map_err(|_| anyhow!("WebView 状态锁定失败"))? = Some(webview.proxy());
    webview
        .set_title("边境社区服工具箱")
        .map_err(|error| anyhow!("设置窗口标题失败：{error}"))?
        .set_size(1500, 900)
        .map_err(|error| anyhow!("设置窗口大小失败：{error}"))?
        .navigate_to_string(&render_html())
        .map_err(|error| anyhow!("加载 WebView 页面失败：{error}"))?;
    webview
        .run()
        .map_err(|error| anyhow!("WebView 事件循环失败：{error}"))
}

fn render_html() -> String {
    INDEX_HTML
        .replace("/*__LGGC_CSS__*/", LGGC_CSS)
        .replace("/*__APP_CSS__*/", APP_CSS)
        .replace("/*__APP_JS__*/", APP_JS)
}

fn handle_message(value: Value, webview: WebView, state: Arc<WebAppState>) -> Result<()> {
    let command = value
        .get("command")
        .and_then(Value::as_str)
        .context("缺少 WebView 命令")?;
    let payload = value.get("payload").cloned().unwrap_or(Value::Null);
    let proxy = webview.proxy();

    match command {
        "ready" => initialize(proxy, state),
        "set-target" => set_target(proxy, state, &payload),
        "install" => start_install(proxy, state),
        "cancel-install" => cancel_install(proxy, state),
        "uninstall" => start_uninstall(proxy, state),
        "launch-pvp" => start_launch(proxy, state, LaunchMode::Pvp),
        "launch-pve" => start_launch(proxy, state, LaunchMode::Pve),
        "detect-processes" => start_detect_processes(proxy, state),
        "stop-processes" => start_stop_processes(proxy, state),
        "refresh-ports" => start_refresh_ports(proxy, state),
        "set-proxy" => set_proxy(proxy, state, &payload),
        "window-minimize" => {
            webview.minimize_window();
            Ok(())
        }
        "window-close" => {
            webview.close_window();
            Ok(())
        }
        "check-update" => {
            emit(
                &proxy,
                json!({
                    "type": "dialog",
                    "title": "检查更新",
                    "message": "WebView 版更新检查还未接入，当前版本：19.20.0。"
                }),
            );
            Ok(())
        }
        unknown => bail!("未知命令：{unknown}"),
    }
}

fn initialize(proxy: WebViewProxy, state: Arc<WebAppState>) -> Result<()> {
    emit(
        &proxy,
        json!({
            "type": "state",
            "appVersion": APP_VERSION,
            "isAdmin": is_running_as_administrator(),
            "proxy": DEFAULT_GITHUB_PROXY_PREFIX,
            "payload": state.core.payload_label(),
        }),
    );

    let core = state.core.clone();
    let target = state.target.clone();
    thread::spawn(move || {
        emit(
            &proxy,
            json!({ "type": "status", "text": "正在检测 Steam 目录..." }),
        );
        match core.detect_steam_game_win64() {
            Ok((path, source)) => {
                if let Ok(mut slot) = target.write() {
                    *slot = Some(path.clone());
                }
                emit(
                    &proxy,
                    json!({
                        "type": "target",
                        "path": path.display().to_string(),
                        "source": source,
                    }),
                );
                emit(&proxy, json!({ "type": "status", "text": "就绪" }));
            }
            Err(error) => {
                emit(
                    &proxy,
                    json!({
                        "type": "status",
                        "text": "未锁定目标目录",
                        "detail": error.to_string(),
                    }),
                );
            }
        }
    });
    Ok(())
}

fn set_target(proxy: WebViewProxy, state: Arc<WebAppState>, payload: &Value) -> Result<()> {
    let raw = payload
        .get("path")
        .and_then(Value::as_str)
        .context("请输入目标目录")?;
    let path = state.core.normalize_selected_path(raw)?;
    *state
        .target
        .write()
        .map_err(|_| anyhow!("目标目录状态锁定失败"))? = Some(path.clone());
    emit(
        &proxy,
        json!({
            "type": "target",
            "path": path.display().to_string(),
            "source": "手动设置",
        }),
    );
    Ok(())
}

fn start_install(proxy: WebViewProxy, state: Arc<WebAppState>) -> Result<()> {
    ensure_admin()?;
    let target = selected_target(&state)?;
    let core = state.core.clone();
    let cancel = InstallCancelToken::new();
    *state
        .install_cancel
        .lock()
        .map_err(|_| anyhow!("安装取消状态锁定失败"))? = Some(cancel.clone());

    emit(
        &proxy,
        json!({
            "type": "progress",
            "open": true,
            "title": "正在安装",
            "detail": "准备安装依赖和文件。",
            "value": 0.0,
        }),
    );

    let cancel_slot = state.install_cancel.clone();
    thread::spawn(move || {
        let progress_proxy = proxy.clone();
        let progress = Arc::new(move |progress: InstallProgress| {
            emit(
                &progress_proxy,
                json!({
                    "type": "progress",
                    "open": true,
                    "title": progress.title,
                    "detail": progress.detail,
                    "value": progress.value,
                }),
            );
        });
        let result = core.install_with_progress(&target, progress, cancel);
        if let Ok(mut slot) = cancel_slot.lock() {
            *slot = None;
        }
        match result {
            Ok(message) => emit(
                &proxy,
                json!({
                    "type": "action-result",
                    "title": "安装完成",
                    "message": message,
                    "progressClosed": true,
                }),
            ),
            Err(error) => emit(
                &proxy,
                json!({
                    "type": "action-result",
                    "title": "安装失败",
                    "message": error.to_string(),
                    "progressClosed": true,
                    "failed": true,
                }),
            ),
        }
    });
    Ok(())
}

fn cancel_install(proxy: WebViewProxy, state: Arc<WebAppState>) -> Result<()> {
    let Some(cancel) = state
        .install_cancel
        .lock()
        .map_err(|_| anyhow!("安装取消状态锁定失败"))?
        .clone()
    else {
        emit(
            &proxy,
            json!({
                "type": "toast",
                "message": "当前没有正在运行的安装任务。"
            }),
        );
        return Ok(());
    };
    cancel.cancel();
    emit(
        &proxy,
        json!({
            "type": "progress",
            "open": true,
            "title": "正在取消",
            "detail": "已发送取消请求，当前步骤结束后会停止。",
            "value": 1.0,
        }),
    );
    Ok(())
}

fn start_uninstall(proxy: WebViewProxy, state: Arc<WebAppState>) -> Result<()> {
    ensure_admin()?;
    let target = selected_target(&state)?;
    let core = state.core.clone();
    run_background(proxy, "卸载", move || core.uninstall(&target));
    Ok(())
}

fn start_launch(proxy: WebViewProxy, state: Arc<WebAppState>, mode: LaunchMode) -> Result<()> {
    let target = selected_target(&state)?;
    let core = state.core.clone();
    run_background(proxy, mode.display_name(), move || {
        core.launch(&target, mode)
    });
    Ok(())
}

fn start_detect_processes(proxy: WebViewProxy, state: Arc<WebAppState>) -> Result<()> {
    let target = selected_target(&state)?;
    let core = state.core.clone();
    run_background(proxy, "进程检测", move || {
        let snapshot = core.collect_runtime_processes(&target)?;
        Ok(format_runtime_snapshot(&snapshot))
    });
    Ok(())
}

fn start_stop_processes(proxy: WebViewProxy, state: Arc<WebAppState>) -> Result<()> {
    let target = selected_target(&state)?;
    let core = state.core.clone();
    run_background(proxy, "关闭相关进程", move || {
        core.stop_runtime_processes(&target)
    });
    Ok(())
}

fn start_refresh_ports(proxy: WebViewProxy, state: Arc<WebAppState>) -> Result<()> {
    let target = state.target.read().ok().and_then(|guard| guard.clone());
    let core = state.core.clone();
    thread::spawn(
        move || match core.port_status_rows_for_target(target.as_deref()) {
            Ok(rows) => emit(
                &proxy,
                json!({
                    "type": "ports",
                    "rows": rows_to_json(rows),
                }),
            ),
            Err(error) => emit(
                &proxy,
                json!({
                    "type": "dialog",
                    "title": "端口检测失败",
                    "message": error.to_string(),
                }),
            ),
        },
    );
    Ok(())
}

fn set_proxy(proxy: WebViewProxy, state: Arc<WebAppState>, payload: &Value) -> Result<()> {
    let value = payload
        .get("value")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_GITHUB_PROXY_PREFIX);
    state.core.set_github_proxy_prefix(value);
    emit(
        &proxy,
        json!({
            "type": "toast",
            "message": format!("下载代理已设置为：{}", state.core.github_proxy_prefix()),
        }),
    );
    Ok(())
}

fn run_background<F>(proxy: WebViewProxy, title: &'static str, task: F)
where
    F: FnOnce() -> Result<String> + Send + 'static,
{
    emit(
        &proxy,
        json!({
            "type": "busy",
            "busy": true,
            "title": title,
        }),
    );
    thread::spawn(move || {
        let result = task();
        emit(
            &proxy,
            json!({
                "type": "busy",
                "busy": false,
                "title": title,
            }),
        );
        match result {
            Ok(message) => emit(
                &proxy,
                json!({
                    "type": "action-result",
                    "title": format!("{title}完成"),
                    "message": message,
                }),
            ),
            Err(error) => emit(
                &proxy,
                json!({
                    "type": "action-result",
                    "title": format!("{title}失败"),
                    "message": error.to_string(),
                    "failed": true,
                }),
            ),
        }
    });
}

fn selected_target(state: &WebAppState) -> Result<PathBuf> {
    state
        .target
        .read()
        .map_err(|_| anyhow!("目标目录状态锁定失败"))?
        .clone()
        .context("请先设置 Boundary 的 Binaries\\Win64 目录")
}

fn ensure_admin() -> Result<()> {
    if is_running_as_administrator() {
        Ok(())
    } else {
        bail!("未使用管理员模式启动，安装和卸载操作已禁止。")
    }
}

fn rows_to_json(rows: Vec<PortStatusRow>) -> Vec<Value> {
    rows.into_iter()
        .map(|row| {
            if let Some(conflict) = row.conflict {
                json!({
                    "protocol": row.protocol,
                    "port": row.port,
                    "status": "占用",
                    "pid": conflict.pid,
                    "name": conflict.name,
                    "expected": conflict.expected,
                })
            } else {
                json!({
                    "protocol": row.protocol,
                    "port": row.port,
                    "status": "空闲",
                    "pid": Value::Null,
                    "name": "",
                    "expected": false,
                })
            }
        })
        .collect()
}

fn format_runtime_snapshot(snapshot: &RuntimeSnapshot) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "游戏进程 {} 个；服务包装器 {} 个；登录服务器 {} 个。",
        snapshot.game.len(),
        snapshot.wrapper.len(),
        snapshot.server.len()
    ));
    append_process_lines(&mut lines, "游戏", &snapshot.game);
    append_process_lines(&mut lines, "服务包装器", &snapshot.wrapper);
    append_process_lines(&mut lines, "登录服务器", &snapshot.server);
    lines.join("\n")
}

fn append_process_lines(lines: &mut Vec<String>, label: &str, processes: &[RuntimeProcess]) {
    for process in processes {
        lines.push(format!(
            "{label}: {} PID {} @ {}",
            process.name, process.pid, process.exe
        ));
    }
}

fn emit(proxy: &WebViewProxy, event: Value) {
    let script = format!(
        "window.boundaryNative && window.boundaryNative.onHostEvent({});",
        event
    );
    let _ = proxy.eval(script);
}
