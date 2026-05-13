#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! 程序入口。WebView UI 控制逻辑放在 web_app 模块，核心安装逻辑放在 core 模块。

mod core;
mod vnt_platform;
mod web_app;
mod webview_host;

fn main() -> anyhow::Result<()> {
    web_app::run()
}
