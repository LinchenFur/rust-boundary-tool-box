#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! 程序入口。具体 UI 控制逻辑放在 app 模块，核心安装逻辑放在 core 模块。

mod app;
mod core;
mod vnt_platform;
mod win;

slint::include_modules!();

fn main() -> anyhow::Result<()> {
    app::run()
}
