//! GitHub 下载代理自动测速选择。

use std::collections::VecDeque;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use serde::Deserialize;

use super::*;

const GITHUB_PROXY_API_URL: &str = "https://api.akams.cn/github";
const PROXY_PING_WORKERS: usize = 64;
const PROXY_PING_TIMEOUT: Duration = Duration::from_millis(2500);

#[derive(Debug, Deserialize)]
struct GithubProxyResponse {
    code: i32,
    #[serde(default)]
    data: Vec<GithubProxyNode>,
}

#[derive(Debug, Deserialize)]
struct GithubProxyNode {
    url: String,
}

/// 一次自动测速后的代理选择结果。
#[derive(Debug, Clone)]
pub struct GithubProxySelection {
    pub prefix: String,
    pub latency_ms: Option<u128>,
    pub tested_count: usize,
    pub reachable_count: usize,
}

impl GithubProxySelection {
    /// 用于进度弹窗和日志的短标签。
    pub fn display_label(&self) -> String {
        if self.prefix.trim().is_empty() {
            return "直连 GitHub".to_string();
        }
        match self.latency_ms {
            Some(ms) => format!("{} ({ms} ms)", self.prefix),
            None => self.prefix.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct GithubProxyPing {
    prefix: String,
    latency_ms: Option<u128>,
}

/// 拉取所有公开代理节点并并发测速，返回当前下载最适合使用的节点。
pub fn select_fastest_github_proxy(current: &str, probe_url: &str) -> GithubProxySelection {
    let candidates = github_proxy_candidates(current);
    if candidates.is_empty() {
        return GithubProxySelection {
            prefix: DEFAULT_GITHUB_PROXY_PREFIX.to_string(),
            latency_ms: None,
            tested_count: 0,
            reachable_count: 0,
        };
    }

    let tested_count = candidates.len();
    let pings = ping_github_proxies(candidates, probe_url);
    let reachable = pings
        .iter()
        .filter(|item| item.latency_ms.is_some())
        .count();
    if let Some(best) = pings
        .iter()
        .filter_map(|item| item.latency_ms.map(|ms| (ms, item)))
        .min_by_key(|(ms, _)| *ms)
        .map(|(_, item)| item)
    {
        return GithubProxySelection {
            prefix: best.prefix.clone(),
            latency_ms: best.latency_ms,
            tested_count,
            reachable_count: reachable,
        };
    }

    GithubProxySelection {
        prefix: normalize_github_proxy_prefix(current)
            .if_empty(DEFAULT_GITHUB_PROXY_PREFIX.to_string()),
        latency_ms: None,
        tested_count,
        reachable_count: reachable,
    }
}

fn github_proxy_candidates(current: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    push_proxy_candidate(&mut candidates, current);
    push_proxy_candidate(&mut candidates, DEFAULT_GITHUB_PROXY_PREFIX);
    if let Ok(fetched) = fetch_github_proxy_urls() {
        for url in fetched {
            push_proxy_candidate(&mut candidates, &url);
        }
    }
    candidates
}

fn fetch_github_proxy_urls() -> anyhow::Result<Vec<String>> {
    let client = Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent(format!("boundary-toolbox/{APP_VERSION}"))
        .build()?;
    let text = client
        .get(GITHUB_PROXY_API_URL)
        .send()?
        .error_for_status()?
        .text()?;
    let response = serde_json::from_str::<GithubProxyResponse>(&text)?;
    if response.code != 200 {
        return Ok(Vec::new());
    }
    Ok(response
        .data
        .into_iter()
        .map(|node| node.url)
        .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
        .collect())
}

fn push_proxy_candidate(candidates: &mut Vec<String>, value: &str) {
    let value = normalize_github_proxy_prefix(value);
    if value.is_empty() {
        return;
    }
    if !candidates
        .iter()
        .any(|item| item.eq_ignore_ascii_case(&value))
    {
        candidates.push(value);
    }
}

fn ping_github_proxies(candidates: Vec<String>, probe_url: &str) -> Vec<GithubProxyPing> {
    let task_count = candidates.len();
    let queue = Arc::new(Mutex::new(
        candidates.into_iter().enumerate().collect::<VecDeque<_>>(),
    ));
    let results = Arc::new(Mutex::new(vec![None; task_count]));
    let worker_count = task_count.clamp(1, PROXY_PING_WORKERS);
    let mut handles = Vec::with_capacity(worker_count);

    for _ in 0..worker_count {
        let queue = Arc::clone(&queue);
        let results = Arc::clone(&results);
        let probe_url = probe_url.to_string();
        handles.push(thread::spawn(move || {
            let client = match Client::builder()
                .timeout(PROXY_PING_TIMEOUT)
                .user_agent(format!("boundary-toolbox/{APP_VERSION}"))
                .redirect(reqwest::redirect::Policy::limited(3))
                .build()
            {
                Ok(client) => client,
                Err(_) => return,
            };

            loop {
                let task = {
                    let mut queue = match queue.lock() {
                        Ok(queue) => queue,
                        Err(_) => return,
                    };
                    queue.pop_front()
                };
                let Some((index, prefix)) = task else {
                    return;
                };
                let latency_ms = ping_github_proxy(&client, &prefix, &probe_url);
                if let Ok(mut results) = results.lock() {
                    results[index] = Some(GithubProxyPing { prefix, latency_ms });
                }
            }
        }));
    }

    for handle in handles {
        let _ = handle.join();
    }

    match Arc::try_unwrap(results) {
        Ok(results) => results.into_inner().unwrap_or_default(),
        Err(results) => results
            .lock()
            .map(|results| results.clone())
            .unwrap_or_default(),
    }
    .into_iter()
    .flatten()
    .collect()
}

fn ping_github_proxy(client: &Client, proxy_prefix: &str, probe_url: &str) -> Option<u128> {
    let probe_url = proxied_github_url(proxy_prefix, probe_url);
    let started = Instant::now();
    match client
        .get(probe_url)
        .header(reqwest::header::RANGE, "bytes=0-3")
        .send()
    {
        Ok(response) => response_counts_as_binary(response).then(|| started.elapsed().as_millis()),
        Err(_) => None,
    }
}

fn response_counts_as_binary(mut response: Response) -> bool {
    if !status_counts_as_reachable(response.status()) {
        return false;
    }

    let headers = response.headers();
    let content_type = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if content_type.contains("text/html")
        || content_type.contains("text/plain")
        || content_type.contains("application/json")
    {
        return false;
    }

    let content_length = headers
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if content_length.is_some_and(|length| length > 4) {
        return true;
    }

    let mut head = [0_u8; 4];
    response.read(&mut head).is_ok_and(|count| count > 0)
}

fn status_counts_as_reachable(status: StatusCode) -> bool {
    status == StatusCode::OK || status == StatusCode::PARTIAL_CONTENT
}

trait IfEmpty {
    fn if_empty(self, fallback: String) -> String;
}

impl IfEmpty for String {
    fn if_empty(self, fallback: String) -> String {
        if self.is_empty() { fallback } else { self }
    }
}
