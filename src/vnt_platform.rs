//! “联机”页面使用的原生 VNT 集成。
//!
//! 本地并入的 VNT v2 源码以 Rust crate 形式链接。这个封装让面向 UI 的
//! 对外 API 保持很小：带选项启动、发出快照/事件、通过 oneshot channel 停止。
//! 整个流程不涉及 web UI 或 webview。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tokio::sync::oneshot;
use vnt_core::context::config::Config;
use vnt_core::core::{NetworkManager, RegisterResponse};
use vnt_core::tls::verifier::CertValidationMode;
use vnt_core::tunnel_core::server::transport::config::ProtocolAddress;
use vnt_core::utils::task_control::TaskGroupManager;
use vnt_ipc as vnt_core;

/// 用户提供给 VNT 核心的启动选项。
#[derive(Debug, Clone)]
pub struct VntLaunchOptions {
    pub server_text: String,
    pub network_code: String,
    pub password: String,
    pub no_tun: bool,
    pub compress: bool,
    pub rtx: bool,
    pub no_punch: bool,
}

/// 在 UI 中展示的单个节点/客户端行。
#[derive(Debug, Clone)]
pub struct VntPeer {
    pub name: String,
    pub address: String,
    pub detail: String,
    pub online: bool,
}

/// 当前 VNT 网络状态的 UI 快照。
#[derive(Debug, Clone)]
pub struct VntSnapshot {
    pub running: bool,
    pub busy: bool,
    pub status: String,
    pub detail: String,
    pub network_code: String,
    pub virtual_ip: String,
    pub server: String,
    pub nat: String,
    pub peer_summary: String,
    pub peers: Vec<VntPeer>,
}

/// 由 VNT 工作线程发给 Slint 控制器的事件。
#[derive(Debug, Clone)]
pub enum VntEvent {
    Snapshot(VntSnapshot),
    Failed(String),
    Stopped(String),
}

type EventSink = Arc<dyn Fn(VntEvent) + Send + Sync + 'static>;

/// 正在运行的 VNT 会话句柄。
///
/// 丢弃该对象会请求关闭，避免窗口关闭后原生核心继续存活。
pub struct VntSession {
    stop_tx: Option<oneshot::Sender<()>>,
}

impl VntSession {
    /// 在独立 OS 线程上启动 VNT，并使用自己的 Tokio runtime。
    pub fn start(options: VntLaunchOptions, sink: EventSink) -> Result<Self> {
        validate_options(&options)?;
        let (stop_tx, stop_rx) = oneshot::channel();
        let thread_sink = sink.clone();

        thread::Builder::new()
            .name("boundary-vnt".to_string())
            .spawn(move || {
                let result = run_vnt_thread(options, stop_rx, thread_sink.clone());
                if let Err(error) = result {
                    thread_sink(VntEvent::Failed(error.to_string()));
                }
            })
            .context("启动 VNT 后台线程失败")?;

        Ok(Self {
            stop_tx: Some(stop_tx),
        })
    }

    /// 请求优雅关闭；最终状态会通过 VntEvent::Stopped 返回。
    pub fn stop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
    }
}

impl Drop for VntSession {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 首次渲染和关闭后使用的未连接状态。
pub fn idle_snapshot() -> VntSnapshot {
    VntSnapshot {
        running: false,
        busy: false,
        status: "未连接".to_string(),
        detail: "填写网络编号后启动联机平台".to_string(),
        network_code: "-".to_string(),
        virtual_ip: "-".to_string(),
        server: "-".to_string(),
        nat: "-".to_string(),
        peer_summary: "0 个节点".to_string(),
        peers: vec![VntPeer {
            name: "暂无联机节点".to_string(),
            address: "启动联机后会显示同网络设备".to_string(),
            detail: "VNT 原生核心未运行".to_string(),
            online: false,
        }],
    }
}

/// 按常见分隔符拆分用户输入的服务器列表。
pub fn split_servers(raw: &str) -> Vec<String> {
    raw.split(['\n', '\r', '\t', ' ', ',', ';', '，', '；'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// 在创建后台线程前校验必填字段。
fn validate_options(options: &VntLaunchOptions) -> Result<()> {
    if split_servers(&options.server_text).is_empty() {
        bail!("请填写 VNT 服务器地址。");
    }
    if options.network_code.trim().is_empty() {
        bail!("请填写网络编号。");
    }
    if options.network_code.trim().chars().count() > 32 {
        bail!("网络编号最多 32 个字符。");
    }
    Ok(())
}

/// 持有 Tokio runtime，使 Slint UI 线程保持同步模型。
fn run_vnt_thread(
    options: VntLaunchOptions,
    stop_rx: oneshot::Receiver<()>,
    sink: EventSink,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("boundary-vnt-runtime")
        .build()
        .context("创建 VNT Tokio runtime 失败")?;

    runtime.block_on(run_vnt(options, stop_rx, sink))
}

/// 运行 VNT 核心生命周期：配置、注册、启动 TUN、轮询状态。
async fn run_vnt(
    options: VntLaunchOptions,
    mut stop_rx: oneshot::Receiver<()>,
    sink: EventSink,
) -> Result<()> {
    sink(VntEvent::Snapshot(VntSnapshot {
        busy: true,
        status: "连接中".to_string(),
        detail: "正在初始化 VNT 原生核心".to_string(),
        network_code: options.network_code.trim().to_string(),
        server: split_servers(&options.server_text).join(", "),
        ..idle_snapshot()
    }));

    if !options.no_tun {
        // 在 Windows 上，VNT 会从进程目录加载 wintun.dll。
        extract_wintun_dll().context("准备 wintun.dll 失败")?;
    }

    let config = build_config(&options).context("VNT 配置无效")?;
    let task_group_manager = TaskGroupManager::new();
    let (task_group, task_group_guard) = task_group_manager
        .create_task()
        .context("创建 VNT 任务组失败")?;
    let mut network_manager = NetworkManager::create_network(Box::new(config), task_group.clone())
        .await
        .context("创建 VNT 网络失败")?;
    let api = network_manager.vnt_api();

    sink(VntEvent::Snapshot(VntSnapshot {
        busy: true,
        status: "注册中".to_string(),
        detail: "正在连接服务器并注册虚拟网络".to_string(),
        network_code: options.network_code.trim().to_string(),
        server: split_servers(&options.server_text).join(", "),
        ..idle_snapshot()
    }));

    let reg_msg = loop {
        tokio::select! {
            _ = &mut stop_rx => {
                task_group.stop();
                sink(VntEvent::Stopped("已取消启动".to_string()));
                return Ok(());
            }
            result = network_manager.register() => {
                match result {
                    Ok(RegisterResponse::Success(reg_msg)) => break reg_msg,
                    Ok(RegisterResponse::Failed(error)) => {
                        bail!("注册失败：{}", error.message);
                    }
                    Err(error) => {
                        sink(VntEvent::Snapshot(VntSnapshot {
                            busy: true,
                            status: "重试中".to_string(),
                            detail: format!("注册失败，5 秒后重试：{error}"),
                            network_code: options.network_code.trim().to_string(),
                            server: split_servers(&options.server_text).join(", "),
                            ..idle_snapshot()
                        }));
                        tokio::select! {
                            _ = &mut stop_rx => {
                                task_group.stop();
                                sink(VntEvent::Stopped("已取消启动".to_string()));
                                return Ok(());
                            }
                            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                        }
                    }
                }
            }
        }
    };

    if !network_manager.is_no_tun() {
        // 注册后才会拿到虚拟 IP，之后才能启动本地网卡并分配网络地址。
        sink(VntEvent::Snapshot(VntSnapshot {
            busy: true,
            status: "创建网卡".to_string(),
            detail: format!("已获得虚拟 IP {}，正在配置虚拟网卡", reg_msg.ip),
            network_code: options.network_code.trim().to_string(),
            virtual_ip: reg_msg.ip.to_string(),
            server: split_servers(&options.server_text).join(", "),
            ..idle_snapshot()
        }));
        network_manager
            .start_tun()
            .await
            .context("创建 TUN 虚拟网卡失败")?;
        network_manager
            .set_tun_network_ip(reg_msg.ip, reg_msg.prefix_len)
            .await
            .context("设置虚拟网卡 IP 失败")?;
    }

    sink(VntEvent::Snapshot(snapshot_from_api(
        &api,
        false,
        "已联机",
        "VNT 虚拟局域网运行中",
    )));

    let status_api = api.clone();
    let status_sink = sink.clone();
    let status_handle = tokio::spawn(async move {
        // 定期轮询 VNT API，让 UI 模型保持最新。
        loop {
            status_sink(VntEvent::Snapshot(snapshot_from_api(
                &status_api,
                false,
                "已联机",
                "VNT 虚拟局域网运行中",
            )));
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });

    tokio::select! {
        _ = network_manager.wait_all_stopped() => {}
        _ = &mut stop_rx => {
            sink(VntEvent::Snapshot(VntSnapshot {
                busy: true,
                status: "停止中".to_string(),
                detail: "正在关闭 VNT 联机核心".to_string(),
                ..snapshot_from_api(&api, true, "停止中", "正在关闭 VNT 联机核心")
            }));
            task_group.stop();
            network_manager.wait_all_stopped().await;
        }
    }

    // 在丢弃 NetworkManager 前先停止轮询任务，避免快照读取和清理过程竞争。
    status_handle.abort();
    let _ = status_handle.await;
    drop(network_manager);
    drop(task_group_guard);
    sink(VntEvent::Stopped("联机核心已停止".to_string()));
    Ok(())
}

/// 将 UI 选项转换为 VNT 核心需要的配置。
fn build_config(options: &VntLaunchOptions) -> Result<Config> {
    let server_addr = split_servers(&options.server_text)
        .into_iter()
        .map(|server| {
            server
                .parse::<ProtocolAddress>()
                .map_err(|error| anyhow!("服务器地址 `{server}` 无效：{error}"))
        })
        .collect::<Result<Vec<_>>>()?;

    let password = if options.password.trim().is_empty() {
        None
    } else {
        Some(options.password.trim().to_string())
    };

    Ok(Config {
        server_addr,
        network_code: options.network_code.trim().to_string(),
        device_id: vnt_core::utils::device_id::get_device_id().context("读取设备 ID 失败")?,
        device_name: default_device_name(),
        tun_name: Some("boundary-vnt".to_string()),
        password,
        // 内置 VNT 默认以该模式连接公共社区服务器。
        cert_mode: CertValidationMode::InsecureSkipVerification,
        no_punch: options.no_punch,
        compress: options.compress,
        rtx: options.rtx,
        no_tun: options.no_tun,
        ..Default::default()
    })
}

/// 从 VNT API 读取实时状态，并整理成 UI 所需结构。
fn snapshot_from_api(
    api: &vnt_core::api::VntApi,
    busy: bool,
    status: &str,
    detail: &str,
) -> VntSnapshot {
    let config = api.get_config();
    let network = api.network();
    let clients = api.client_ips();
    let direct_count = clients
        .iter()
        .filter(|client| api.is_direct(&client.ip))
        .count();
    let online_count = clients.iter().filter(|client| client.online).count();
    let server_nodes = api.server_node_list();
    let connected_servers = server_nodes
        .iter()
        .filter(|server| server.connected)
        .count();
    let nat_info = api.nat_info();

    let peers = if clients.is_empty() {
        vec![VntPeer {
            name: "暂无联机节点".to_string(),
            address: "等待同网络设备上线".to_string(),
            detail: "同一网络编号的设备会显示在这里".to_string(),
            online: false,
        }]
    } else {
        clients
            .into_iter()
            .map(|client| {
                // 路由和 RTT 是尽力而为的诊断信息，缺失时不应导致整个快照失败。
                let rtt = api
                    .get_rtt(&client.ip)
                    .map(|value| format!("{value} ms"))
                    .unwrap_or_else(|| "-".to_string());
                let route = api
                    .find_route(&client.ip)
                    .map(|route| route.route_key().to_string())
                    .unwrap_or_else(|| "经服务器转发".to_string());
                VntPeer {
                    name: client.ip.to_string(),
                    address: if api.is_direct(&client.ip) {
                        "P2P 直连".to_string()
                    } else {
                        "服务器转发".to_string()
                    },
                    detail: format!("延迟 {rtt} / 路由 {route}"),
                    online: client.online,
                }
            })
            .collect()
    };

    VntSnapshot {
        running: status != "未连接",
        busy,
        status: status.to_string(),
        detail: detail.to_string(),
        network_code: config
            .as_ref()
            .map(|config| config.network_code.clone())
            .unwrap_or_else(|| "-".to_string()),
        virtual_ip: network
            .map(|network| format!("{}/{}", network.ip, network.prefix_len))
            .unwrap_or_else(|| "-".to_string()),
        server: if server_nodes.is_empty() {
            config
                .as_ref()
                .map(|config| {
                    config
                        .server_addr
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| "-".to_string())
        } else {
            format!("{} / {} 已连接", connected_servers, server_nodes.len())
        },
        nat: nat_info
            .map(|info| {
                let ipv4 = if info.public_ips.is_empty() {
                    "-".to_string()
                } else {
                    info.public_ips
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                format!("{:?} / {}", info.nat_type, ipv4)
            })
            .unwrap_or_else(|| "-".to_string()),
        peer_summary: format!(
            "{} 在线 / {} 直连 / {} 总计",
            online_count,
            direct_count,
            peers
                .iter()
                .filter(|peer| peer.name != "暂无联机节点")
                .count()
        ),
        peers,
    }
}

/// 在 VNT 网络内使用的稳定设备显示名。
fn default_device_name() -> String {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "Windows".to_string());
    format!("Boundary-{host}")
}

#[cfg(windows)]
/// 使用 TUN 模式时，把内置 wintun.dll 解压到可执行文件旁边。
fn extract_wintun_dll() -> Result<()> {
    #[cfg(target_arch = "x86_64")]
    const WINTUN_DLL: &[u8] = include_bytes!("../vendor/vnt/dll/amd64/wintun.dll");
    #[cfg(target_arch = "x86")]
    const WINTUN_DLL: &[u8] = include_bytes!("../vendor/vnt/dll/x86/wintun.dll");
    #[cfg(target_arch = "aarch64")]
    const WINTUN_DLL: &[u8] = include_bytes!("../vendor/vnt/dll/arm64/wintun.dll");
    #[cfg(target_arch = "arm")]
    const WINTUN_DLL: &[u8] = include_bytes!("../vendor/vnt/dll/arm/wintun.dll");

    let path = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("wintun.dll");
    if !path.exists() {
        std::fs::write(&path, WINTUN_DLL)
            .with_context(|| format!("写入 {} 失败", path.display()))?;
    }
    Ok(())
}

#[cfg(not(windows))]
/// 非 Windows 构建不需要内置 Wintun DLL。
fn extract_wintun_dll() -> Result<()> {
    Ok(())
}
