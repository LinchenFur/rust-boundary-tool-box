//! GitHub 代理节点列表获取、测速和设置页模型同步。

use super::*;

use std::collections::VecDeque;
use std::io::Read;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use serde::Deserialize;

const GITHUB_PROXY_API_URL: &str = "https://api.akams.cn/github";
const PROXY_PING_WORKERS: usize = 64;
const PROXY_PING_TIMEOUT: Duration = Duration::from_millis(2500);

const PROXY_STATE_UNTESTED: i32 = 0;
const PROXY_STATE_OK: i32 = 1;
const PROXY_STATE_FAILED: i32 = 2;

#[derive(Debug, Deserialize)]
struct GithubProxyResponse {
    code: i32,
    msg: String,
    #[serde(default)]
    data: Vec<GithubProxyNode>,
    update_time: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubProxyNode {
    url: String,
}

#[derive(Debug, Clone)]
pub(crate) struct GithubProxyOption {
    url: String,
    latency_ms: Option<u128>,
    state: i32,
}

impl GithubProxyOption {
    fn untested(url: String) -> Self {
        Self {
            url,
            latency_ms: None,
            state: PROXY_STATE_UNTESTED,
        }
    }
}

pub(crate) fn initial_github_proxy_rows(current: &str, language: i32) -> Vec<ProxyRow> {
    merge_github_proxy_urls(current, Vec::new())
        .into_iter()
        .map(GithubProxyOption::untested)
        .map(|option| proxy_option_to_row(&option, current, language))
        .collect()
}

pub(crate) fn proxy_untested_row(url: &str, current: &str, language: i32) -> ProxyRow {
    proxy_option_to_row(
        &GithubProxyOption::untested(core::normalize_github_proxy_prefix(url)),
        current,
        language,
    )
}

pub(crate) fn proxy_options_to_rows(
    options: &[GithubProxyOption],
    current: &str,
    language: i32,
) -> Vec<ProxyRow> {
    options
        .iter()
        .map(|option| proxy_option_to_row(option, current, language))
        .collect()
}

fn proxy_option_to_row(option: &GithubProxyOption, current: &str, language: i32) -> ProxyRow {
    let current = core::normalize_github_proxy_prefix(current);
    let url = core::normalize_github_proxy_prefix(&option.url);
    let current = !current.is_empty() && url.eq_ignore_ascii_case(&current);
    ProxyRow {
        url: url.into(),
        latency: proxy_latency_text(option.state, option.latency_ms, language).into(),
        state: option.state,
        current,
    }
}

fn proxy_latency_text(state: i32, latency_ms: Option<u128>, language: i32) -> String {
    match (state, latency_ms) {
        (PROXY_STATE_OK, Some(ms)) => format!("{ms} ms"),
        (PROXY_STATE_FAILED, _) => {
            i18n::tr(language, "超时", "Timeout", "タイムアウト").to_string()
        }
        _ => i18n::tr(language, "未测速", "Untested", "未測定").to_string(),
    }
}

fn merge_github_proxy_urls(current: &str, fetched: Vec<String>) -> Vec<String> {
    let mut options = Vec::new();
    push_proxy_option(&mut options, current);
    push_proxy_option(&mut options, core::DEFAULT_GITHUB_PROXY_PREFIX);
    for item in fetched {
        push_proxy_option(&mut options, &item);
    }
    options
}

fn push_proxy_option(options: &mut Vec<String>, value: &str) {
    let value = core::normalize_github_proxy_prefix(value);
    if value.is_empty() {
        return;
    }
    if !options
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(&value))
    {
        options.push(value);
    }
}

pub(crate) fn fetch_github_proxy_options(
    current: &str,
) -> Result<(Vec<GithubProxyOption>, usize, Option<String>)> {
    let client = Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent(format!("boundary-toolbox/{}", APP_VERSION))
        .build()
        .context("创建 GitHub 代理列表 HTTP 客户端失败")?;
    let text = client
        .get(GITHUB_PROXY_API_URL)
        .send()
        .context("请求 GitHub 代理列表失败")?
        .error_for_status()
        .context("GitHub 代理列表接口返回错误")?
        .text()
        .context("读取 GitHub 代理列表响应失败")?;
    let response = serde_json::from_str::<GithubProxyResponse>(&text)
        .context("解析 GitHub 代理列表响应失败")?;
    if response.code != 200 {
        anyhow::bail!("GitHub 代理列表接口返回失败：{}", response.msg);
    }
    let fetched = response
        .data
        .into_iter()
        .map(|node| node.url)
        .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
        .collect::<Vec<_>>();
    let fetched_count = fetched.len();
    let urls = merge_github_proxy_urls(current, fetched);
    let rows = ping_github_proxy_options(urls);
    Ok((rows, fetched_count, response.update_time))
}

fn ping_github_proxy_options(urls: Vec<String>) -> Vec<GithubProxyOption> {
    if urls.is_empty() {
        return Vec::new();
    }

    let task_count = urls.len();
    let queue = Arc::new(Mutex::new(
        urls.into_iter().enumerate().collect::<VecDeque<_>>(),
    ));
    let results = Arc::new(Mutex::new(vec![None; task_count]));
    let worker_count = task_count.min(PROXY_PING_WORKERS).max(1);
    let mut handles = Vec::with_capacity(worker_count);

    for _ in 0..worker_count {
        let queue = Arc::clone(&queue);
        let results = Arc::clone(&results);
        handles.push(thread::spawn(move || {
            let client = match Client::builder()
                .timeout(PROXY_PING_TIMEOUT)
                .user_agent(format!("boundary-toolbox/{}", APP_VERSION))
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
                let Some((index, url)) = task else {
                    return;
                };
                let result = ping_github_proxy(&client, &url);
                let option = match result {
                    Some(ms) => GithubProxyOption {
                        url,
                        latency_ms: Some(ms),
                        state: PROXY_STATE_OK,
                    },
                    None => GithubProxyOption {
                        url,
                        latency_ms: None,
                        state: PROXY_STATE_FAILED,
                    },
                };
                if let Ok(mut results) = results.lock() {
                    results[index] = Some(option);
                }
            }
        }));
    }

    for handle in handles {
        let _ = handle.join();
    }

    let mut results = match Arc::try_unwrap(results) {
        Ok(results) => results.into_inner().unwrap_or_default(),
        Err(results) => results
            .lock()
            .map(|results| results.clone())
            .unwrap_or_default(),
    };
    results
        .drain(..)
        .flatten()
        .collect::<Vec<GithubProxyOption>>()
}

fn ping_github_proxy(client: &Client, proxy_prefix: &str) -> Option<u128> {
    let proxy_prefix = core::normalize_github_proxy_prefix(proxy_prefix);
    if proxy_prefix.is_empty() {
        return None;
    }
    let probe_url = format!("{proxy_prefix}{}", core::PROJECT_REBOUND_RELEASE_URL);
    let started = Instant::now();
    match client
        .get(&probe_url)
        .header(reqwest::header::RANGE, "bytes=0-3")
        .send()
    {
        Ok(response) => {
            response_counts_as_project_rebound_zip(response).then(|| started.elapsed().as_millis())
        }
        _ => None,
    }
}

fn response_counts_as_project_rebound_zip(mut response: Response) -> bool {
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
    if content_length.is_some_and(|length| length > 1024) {
        return true;
    }

    let mut head = [0_u8; 4];
    response
        .read_exact(&mut head)
        .is_ok_and(|_| head == [b'P', b'K', 0x03, 0x04])
}

fn status_counts_as_reachable(status: StatusCode) -> bool {
    status.is_success()
}

impl AppController {
    /// 从 github.akams.cn 对应 API 刷新 GitHub 代理节点列表并测速。
    pub(super) fn start_refresh_github_proxy_list(&mut self) {
        if self.ui.get_github_proxy_loading() {
            return;
        }
        self.ui.set_github_proxy_loading(true);
        self.ui.set_github_proxy_status_text(
            self.tr(
                "代理列表：多线程测速中...",
                "Proxy list: testing with multiple threads...",
                "プロキシ一覧: マルチスレッド測定中...",
            )
            .into(),
        );
        let tx = self.tx.clone();
        let current = self.ui.get_github_proxy_text().to_string();
        thread::spawn(move || match fetch_github_proxy_options(&current) {
            Ok((rows, fetched_count, update_time)) => {
                let _ = tx.send(AppMessage::GithubProxyRows {
                    rows,
                    fetched_count,
                    update_time,
                });
            }
            Err(error) => {
                let _ = tx.send(AppMessage::GithubProxyRowsFailed(error.to_string()));
            }
        });
    }

    /// 原地同步设置页 GitHub 代理弹窗列表。
    pub(super) fn set_github_proxy_rows(&mut self, rows: Vec<ProxyRow>) {
        while self.github_proxy_model.row_count() > rows.len() {
            let _ = self
                .github_proxy_model
                .remove(self.github_proxy_model.row_count() - 1);
        }
        for (index, row) in rows.into_iter().enumerate() {
            if index < self.github_proxy_model.row_count() {
                self.github_proxy_model.set_row_data(index, row);
            } else {
                self.github_proxy_model.push(row);
            }
        }
    }

    pub(super) fn sync_github_proxy_current_selection(&mut self) {
        let current = core::normalize_github_proxy_prefix(&self.ui.get_github_proxy_text());
        let mut has_current = current.is_empty();
        let language = self.language();
        for index in 0..self.github_proxy_model.row_count() {
            let Some(mut row) = self.github_proxy_model.row_data(index) else {
                continue;
            };
            let row_url = core::normalize_github_proxy_prefix(&row.url);
            row.current = !current.is_empty() && row_url.eq_ignore_ascii_case(&current);
            if row.current {
                has_current = true;
            }
            if row.state != PROXY_STATE_OK {
                row.latency = proxy_latency_text(row.state, None, language).into();
            }
            self.github_proxy_model.set_row_data(index, row);
        }

        if !has_current {
            self.github_proxy_model
                .push(proxy_untested_row(&current, &current, language));
        }
    }
}
