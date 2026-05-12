//! AppController 后台消息、日志和列表模型更新。

use super::*;

const MAX_VISIBLE_LOG_LINES: usize = 400;
const INSTALL_DETAIL_COLUMNS: usize = 54;

impl AppController {
    /// 同时向会话日志文件和可见日志面板追加一行。
    pub(super) fn append_log(&mut self, message: &str) {
        let _ = writeln!(self.session_log_file, "{}", message);
        let current = self.ui.get_log_text().to_string();
        let next = if current.is_empty() {
            message.to_string()
        } else {
            format!("{current}\n{message}")
        };
        self.ui.set_log_text(trim_visible_log(&next).into());
    }

    pub(super) fn set_install_progress_detail_text(&self, detail: &str) {
        self.ui.set_install_progress_detail(detail.into());
        self.ui
            .set_install_progress_detail_lines(estimate_dialog_text_lines(
                detail,
                INSTALL_DETAIL_COLUMNS,
            ));
    }

    /// 将队列中的工作线程消息应用到 UI 状态。
    pub(super) fn drain_messages(&mut self) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                AppMessage::Log(line) => self.append_log(&line),
                AppMessage::InstallProgress(progress) => {
                    let value = progress.value.clamp(0.0, 1.0);
                    let (title, detail) = localize_install_progress(&progress, self.language());
                    self.ui.set_install_progress_visible(true);
                    self.ui.set_install_progress_value(value);
                    self.ui
                        .set_install_progress_percent(format!("{:.0}%", value * 100.0).into());
                    self.ui.set_install_progress_title(title.into());
                    self.set_install_progress_detail_text(&detail);
                }
                AppMessage::PortRows(rows) => self.update_port_rows(rows),
                AppMessage::ServerRows(rows) => {
                    let count = rows.len();
                    self.ui.set_servers_loading(false);
                    self.update_server_rows(rows);
                    self.ui.set_server_status_text(
                        format!(
                            "{}{count}{}",
                            self.tr(
                                "服务器列表：已加载 ",
                                "Server list: loaded ",
                                "サーバー一覧: "
                            ),
                            self.tr(" 个", "", " 件を読み込みました"),
                        )
                        .into(),
                    );
                    self.append_log(&format!(
                        "[{}] 已刷新服务器列表：{} 个",
                        core::now_text(),
                        count
                    ));
                }
                AppMessage::ServerRowsFailed(error) => {
                    self.ui.set_servers_loading(false);
                    self.ui.set_server_status_text(
                        self.tr(
                            "服务器列表：刷新失败",
                            "Server list: refresh failed",
                            "サーバー一覧: 更新失敗",
                        )
                        .into(),
                    );
                    self.set_server_rows(vec![server_placeholder_row(
                        self.tr(
                            "服务器列表刷新失败",
                            "Server list refresh failed",
                            "サーバー一覧の更新に失敗しました",
                        ),
                        &error,
                        self.language(),
                    )]);
                    self.append_log(&format!(
                        "[{}] 服务器列表刷新失败：{}",
                        core::now_text(),
                        error
                    ));
                }
                AppMessage::UpdateCheckFinished { result, automatic } => {
                    self.ui.set_update_checking(false);
                    self.ui.set_update_status_text(
                        update_status_text(&result, self.language()).into(),
                    );
                    self.append_log(&format!(
                        "[{}] 更新检查完成：当前 {}，最新 {}",
                        core::now_text(),
                        APP_VERSION,
                        result.latest_tag
                    ));
                    let title = if automatic {
                        self.tr("自动检查更新", "Automatic Update Check", "自動更新チェック")
                    } else {
                        self.tr("检查更新", "Check Updates", "更新を確認")
                    };
                    if result.is_newer && result.asset_url.is_some() {
                        self.show_confirm_dialog(
                            title,
                            &update_dialog_text(&result, self.language()),
                            self.tr("立即更新", "Update Now", "今すぐ更新"),
                            self.tr("稍后", "Later", "後で"),
                            PendingDialogAction::DownloadUpdate { result },
                        );
                    } else {
                        self.show_info_dialog(title, &update_dialog_text(&result, self.language()));
                    }
                }
                AppMessage::UpdateCheckFailed { error, automatic } => {
                    self.ui.set_update_checking(false);
                    self.ui.set_update_status_text(
                        format!(
                            "{}{error}",
                            self.tr(
                                "更新：检查失败：",
                                "Update check failed: ",
                                "更新チェック失敗: ",
                            )
                        )
                        .into(),
                    );
                    self.append_log(&format!("[{}] 更新检查失败：{}", core::now_text(), error));
                    let title = if automatic {
                        self.tr("自动检查更新", "Automatic Update Check", "自動更新チェック")
                    } else {
                        self.tr("检查更新", "Check Updates", "更新を確認")
                    };
                    self.show_error_dialog(title, &error);
                }
                AppMessage::UpdateRestartScheduled { tag } => {
                    self.ui.set_busy(false);
                    self.ui.set_update_checking(false);
                    self.ui.set_update_status_text(
                        format!(
                            "{}{}",
                            self.tr("更新：正在重启 ", "Update restarting: ", "更新再起動中: ",),
                            tag
                        )
                        .into(),
                    );
                    self.append_log(&format!(
                        "[{}] 已安排自动替换并重启：{}",
                        core::now_text(),
                        tag,
                    ));
                    self.ui.set_install_progress_visible(true);
                    self.ui.set_install_progress_cancelable(false);
                    self.ui.set_install_progress_dialog_title(
                        self.tr("立即更新", "Update Now", "今すぐ更新").into(),
                    );
                    self.ui.set_install_progress_value(1.0);
                    self.ui.set_install_progress_percent("100%".into());
                    self.ui.set_install_progress_title(
                        self.tr("准备重启", "Restarting", "再起動準備中").into(),
                    );
                    self.set_install_progress_detail_text(&format!(
                        "{}\n{}",
                        self.tr(
                            "更新文件已下载并通过校验。",
                            "The update file has been downloaded and verified.",
                            "更新ファイルのダウンロードと検証が完了しました。",
                        ),
                        self.tr(
                            "工具箱将关闭，更新助手会替换当前文件并重新启动。",
                            "The toolbox will close; the update helper will replace this executable and restart it.",
                            "ツールボックスを閉じ、更新ヘルパーが現在の実行ファイルを置き換えて再起動します。",
                        )
                    ));
                    self.hide_app_dialog();
                    self.stop_background.store(true, Ordering::Relaxed);
                    let _ = self.ui.hide();
                    let _ = slint::quit_event_loop();
                }
                AppMessage::UpdateDownloadFailed(error) => {
                    self.ui.set_busy(false);
                    self.ui.set_update_checking(false);
                    self.ui.set_update_status_text(
                        format!(
                            "{}{error}",
                            self.tr(
                                "更新：下载失败：",
                                "Update download failed: ",
                                "更新ダウンロード失敗: ",
                            )
                        )
                        .into(),
                    );
                    self.append_log(&format!("[{}] 更新下载失败：{}", core::now_text(), error));
                    self.ui.set_install_progress_visible(true);
                    self.ui.set_install_progress_cancelable(false);
                    self.ui.set_install_progress_dialog_title(
                        self.tr("下载更新", "Download Update", "更新をダウンロード")
                            .into(),
                    );
                    self.ui.set_install_progress_title(
                        self.tr("下载失败", "Download Failed", "ダウンロード失敗")
                            .into(),
                    );
                    self.set_install_progress_detail_text(&error);
                    self.hide_app_dialog();
                }
                AppMessage::GithubProxyRows {
                    rows,
                    fetched_count,
                    update_time,
                } => {
                    self.ui.set_github_proxy_loading(false);
                    let rows = proxy_options_to_rows(
                        &rows,
                        &self.ui.get_github_proxy_text(),
                        self.language(),
                    );
                    self.set_github_proxy_rows(rows);
                    let update_time = update_time.unwrap_or_else(|| "-".to_string());
                    self.ui.set_github_proxy_status_text(
                        format!(
                            "{}{fetched_count}{}{}",
                            self.tr("代理列表：已加载 ", "Proxy list: loaded ", "プロキシ一覧: "),
                            self.tr(" 个，更新时间：", " nodes, updated: ", " 件、更新時刻: "),
                            update_time
                        )
                        .into(),
                    );
                    self.append_log(&format!(
                        "[{}] 已刷新 GitHub 代理列表：{} 个，更新时间 {}",
                        core::now_text(),
                        fetched_count,
                        update_time
                    ));
                }
                AppMessage::GithubProxyRowsFailed(error) => {
                    self.ui.set_github_proxy_loading(false);
                    self.ui.set_github_proxy_status_text(
                        self.tr(
                            "代理列表：刷新失败",
                            "Proxy list: refresh failed",
                            "プロキシ一覧: 更新失敗",
                        )
                        .into(),
                    );
                    self.append_log(&format!(
                        "[{}] GitHub 代理列表刷新失败：{}",
                        core::now_text(),
                        error
                    ));
                }
                AppMessage::VntEvent(event) => self.apply_vnt_event(event),
                AppMessage::ActionFinished {
                    title,
                    status,
                    dialog,
                    process_status,
                    target,
                } => {
                    self.ui.set_busy(false);
                    if title == "安装" {
                        self.install_cancel = None;
                    }
                    self.ui
                        .set_status_text(self.localize_action_status(&status).into());
                    if let Some(process_status) = process_status {
                        self.ui.set_process_status_text(process_status.into());
                    }
                    if let Some(target) = target {
                        let ready = self.tr("已就绪", "Ready", "準備完了");
                        self.set_current_target(Some(target), ready);
                    } else {
                        self.sync_has_target();
                    }
                    let localized_dialog = localize_dialog_text(&dialog, self.language());
                    if title == "安装" {
                        self.ui.set_install_progress_visible(true);
                        self.ui.set_install_progress_cancelable(true);
                        self.ui.set_install_progress_value(1.0);
                        self.ui.set_install_progress_percent("100%".into());
                        self.ui.set_install_progress_title(
                            self.tr("安装完成", "Install complete", "インストール完了")
                                .into(),
                        );
                        self.set_install_progress_detail_text(
                            &localized_dialog.replace('\n', "；"),
                        );
                        // 安装成功后保留进度弹窗，由用户确认关闭。
                        self.append_log(&format!(
                            "[{}] 安装完成：{}",
                            core::now_text(),
                            dialog.replace('\n', "；")
                        ));
                    } else if title == "字体安装" {
                        self.ui.set_install_progress_visible(true);
                        self.ui.set_install_progress_cancelable(false);
                        self.ui.set_install_progress_value(1.0);
                        self.ui.set_install_progress_percent("100%".into());
                        self.ui.set_install_progress_title(
                            self.tr(
                                "字体安装完成",
                                "Font install complete",
                                "フォントインストール完了",
                            )
                            .into(),
                        );
                        self.set_install_progress_detail_text(
                            &localized_dialog.replace('\n', "；"),
                        );
                        self.append_log(&format!(
                            "[{}] 字体安装完成：{}",
                            core::now_text(),
                            dialog.replace('\n', "；")
                        ));
                        self.start_update_check(true);
                    } else {
                        let display_title = self.localize_action_title(&title);
                        self.show_info_dialog(&display_title, &localized_dialog);
                    }
                }
                AppMessage::ActionFailed {
                    title,
                    status,
                    error,
                } => {
                    self.ui.set_busy(false);
                    if title == "安装" {
                        self.install_cancel = None;
                    }
                    self.ui
                        .set_status_text(self.localize_action_status(&status).into());
                    let cancelled = status == "已取消";
                    if title == "安装" {
                        self.ui.set_install_progress_visible(true);
                        self.ui.set_install_progress_title(
                            if cancelled {
                                self.tr(
                                    "安装已取消",
                                    "Install cancelled",
                                    "インストールはキャンセルされました",
                                )
                            } else {
                                self.tr("安装失败", "Install failed", "インストール失敗")
                            }
                            .into(),
                        );
                        if cancelled {
                            self.set_install_progress_detail_text(self.tr(
                                "安装已取消",
                                "Install cancelled",
                                "インストールはキャンセルされました",
                            ));
                        } else {
                            self.set_install_progress_detail_text(&error);
                        }
                    }
                    self.sync_has_target();
                    let display_title = self.localize_action_title(&title);
                    if title == "字体安装" {
                        self.ui.set_install_progress_visible(true);
                        self.ui.set_install_progress_cancelable(false);
                        self.ui.set_install_progress_title(
                            self.tr(
                                "字体安装失败",
                                "Font install failed",
                                "フォントインストール失敗",
                            )
                            .into(),
                        );
                        self.set_install_progress_detail_text(&error);
                        self.show_error_dialog(&display_title, &error);
                        self.start_update_check(true);
                    } else if cancelled {
                        self.append_log(&format!("[{}] 安装已取消。", core::now_text()));
                    } else {
                        self.show_error_dialog(&display_title, &error);
                    }
                }
                AppMessage::ScanFinished { result, dialog } => {
                    self.ui.set_busy(false);
                    if let Some(path) = result {
                        self.mode = PathMode::Manual;
                        self.ui.set_auto_mode(false);
                        self.ui.set_manual_path(path.display().to_string().into());
                        self.ui.set_detected_text(dialog.clone().into());
                        let ready = self.tr("已就绪", "Ready", "準備完了");
                        self.set_current_target(Some(path), ready);
                        self.ui.set_status_text(
                            self.tr(
                                "已找到游戏目录",
                                "Game path found",
                                "ゲームディレクトリが見つかりました",
                            )
                            .into(),
                        );
                    } else {
                        self.ui.set_status_text(
                            self.tr(
                                "未找到游戏目录",
                                "Game path not found",
                                "ゲームディレクトリが見つかりません",
                            )
                            .into(),
                        );
                        self.sync_has_target();
                    }
                    self.ui.set_show_drive_dialog(false);
                    self.show_info_dialog(
                        self.tr("全盘扫描", "Full Scan", "全体スキャン"),
                        &dialog,
                    );
                }
            }
        }
    }

    /// 将成功返回的服务器数据转换为 ListView 行。
    pub(super) fn update_server_rows(&mut self, servers: Vec<RemoteServer>) {
        if servers.is_empty() {
            self.set_server_rows(vec![server_placeholder_row(
                self.tr("暂无服务器", "No servers yet", "サーバーはありません"),
                self.tr(
                    "接口返回了空列表",
                    "The API returned an empty list",
                    "API は空のリストを返しました",
                ),
                self.language(),
            )]);
            return;
        }

        let language = self.language();
        let rows = servers
            .into_iter()
            .map(|server| server_to_row(server, language))
            .collect::<Vec<_>>();
        self.set_server_rows(rows);
    }

    /// 在不替换模型对象的前提下同步服务器模型。
    pub(super) fn set_server_rows(&mut self, rows: Vec<ServerRow>) {
        while self.server_model.row_count() > rows.len() {
            let _ = self.server_model.remove(self.server_model.row_count() - 1);
        }
        for (index, row) in rows.into_iter().enumerate() {
            if index < self.server_model.row_count() {
                self.server_model.set_row_data(index, row);
            } else {
                self.server_model.push(row);
            }
        }
    }

    /// 将 core 的端口诊断结果映射为 Slint 行。
    pub(super) fn update_port_rows(&mut self, rows: Vec<CorePortStatusRow>) {
        let language = self.language();
        let mapped = rows
            .into_iter()
            .map(|row| {
                let protocol = row.protocol.to_string();
                let port = i32::from(row.port);
                let (occupied, expected, pid, detail) = row
                    .conflict
                    .as_ref()
                    .map(|conflict| {
                        let prefix = if conflict.expected {
                            self.tr("目标进程", "Target process", "対象プロセス")
                        } else {
                            self.tr("异常占用", "Unexpected usage", "想定外の使用")
                        };
                        (
                            true,
                            conflict.expected,
                            i32::try_from(conflict.pid).unwrap_or(0),
                            format!("{prefix}：PID {} {}", conflict.pid, conflict.name),
                        )
                    })
                    .unwrap_or_else(|| {
                        (
                            false,
                            false,
                            0,
                            i18n::tr(language, "空闲", "Free", "空き").to_string(),
                        )
                    });
                PortRow {
                    occupied,
                    expected,
                    label: format!("{}/{}", row.protocol, row.port).into(),
                    detail: detail.into(),
                    protocol: protocol.into(),
                    port,
                    pid,
                }
            })
            .collect::<Vec<_>>();

        while self.port_model.row_count() > mapped.len() {
            let _ = self.port_model.remove(self.port_model.row_count() - 1);
        }
        for (index, row) in mapped.into_iter().enumerate() {
            if index < self.port_model.row_count() {
                self.port_model.set_row_data(index, row);
            } else {
                self.port_model.push(row);
            }
        }
    }
}

fn trim_visible_log(text: &str) -> String {
    let line_count = text.lines().count();
    if line_count <= MAX_VISIBLE_LOG_LINES {
        return text.to_string();
    }
    let keep_count = MAX_VISIBLE_LOG_LINES.saturating_sub(1);
    let skip = line_count.saturating_sub(keep_count);
    let mut visible = "... 仅显示最近日志，完整内容见日志文件 ...".to_string();
    for line in text.lines().skip(skip) {
        visible.push('\n');
        visible.push_str(line);
    }
    visible
}

fn localize_install_progress(progress: &InstallProgress, language: i32) -> (String, String) {
    (
        localize_install_text(&progress.title, language),
        localize_install_text(&progress.detail, language),
    )
}

fn localize_install_text(text: &str, language: i32) -> String {
    match text {
        "准备字体" => {
            i18n::tr(language, "准备字体", "Preparing font", "フォント準備中").to_string()
        }
        "未检测到界面字体，准备获取最新字体包。" => i18n::tr(
            language,
            "未检测到界面字体，准备获取最新字体包。",
            "UI font is missing. Preparing the latest font package.",
            "UI フォントが見つからないため、最新のフォントパッケージを準備しています。",
        )
        .to_string(),
        "查询字体版本" => i18n::tr(
            language,
            "查询字体版本",
            "Checking font release",
            "フォントリリース確認中",
        )
        .to_string(),
        "正在读取 Maple Mono 最新 Release。" => i18n::tr(
            language,
            "正在读取 Maple Mono 最新 Release。",
            "Reading the latest Maple Mono release.",
            "Maple Mono の最新 Release を読み込んでいます。",
        )
        .to_string(),
        "校验字体缓存" => i18n::tr(
            language,
            "校验字体缓存",
            "Checking font cache",
            "フォントキャッシュ確認中",
        )
        .to_string(),
        "正在读取字体包校验信息。" => i18n::tr(
            language,
            "正在读取字体包校验信息。",
            "Reading font package checksum information.",
            "フォントパッケージのチェックサム情報を読み込んでいます。",
        )
        .to_string(),
        "下载字体" => i18n::tr(
            language,
            "下载字体",
            "Downloading font",
            "フォントをダウンロード中",
        )
        .to_string(),
        "校验字体包" => i18n::tr(
            language,
            "校验字体包",
            "Verifying font package",
            "フォントパッケージ検証中",
        )
        .to_string(),
        "正在校验字体包 SHA256。" => i18n::tr(
            language,
            "正在校验字体包 SHA256。",
            "Verifying the font package SHA256.",
            "フォントパッケージの SHA256 を検証しています。",
        )
        .to_string(),
        "安装字体" => i18n::tr(
            language,
            "安装字体",
            "Installing font",
            "フォントをインストール中",
        )
        .to_string(),
        "正在解压并注册字体文件。" => i18n::tr(
            language,
            "正在解压并注册字体文件。",
            "Extracting and registering font files.",
            "フォントファイルを展開して登録しています。",
        )
        .to_string(),
        "字体安装完成" => i18n::tr(
            language,
            "字体安装完成",
            "Font install complete",
            "フォントインストール完了",
        )
        .to_string(),
        value if value.starts_with("开始下载 ") => format!(
            "{}{}",
            i18n::tr(
                language,
                "开始下载 ",
                "Starting download: ",
                "ダウンロード開始: "
            ),
            value.trim_start_matches("开始下载 ")
        ),
        value if value.starts_with("字体包：已下载 ") => value
            .replace(
                "字体包：已下载 ",
                i18n::tr(
                    language,
                    "字体包：已下载 ",
                    "Font package: downloaded ",
                    "フォントパッケージ: ダウンロード済み ",
                ),
            )
            .replace(" / ", i18n::tr(language, " / ", " / ", " / ")),
        value if value.starts_with("字体包已缓存：") => format!(
            "{}{}",
            i18n::tr(
                language,
                "字体包已缓存：",
                "Font package cached: ",
                "フォントパッケージキャッシュ: ",
            ),
            value.trim_start_matches("字体包已缓存：")
        ),
        value if value.starts_with("已安装/更新 ") && value.ends_with(" 个字体文件。") => {
            value
                .replace(
                    "已安装/更新 ",
                    i18n::tr(
                        language,
                        "已安装/更新 ",
                        "Installed/updated ",
                        "インストール/更新済み ",
                    ),
                )
                .replace(
                    " 个字体文件。",
                    i18n::tr(
                        language,
                        " 个字体文件。",
                        " font files.",
                        " 個のフォントファイル。",
                    ),
                )
        }
        "准备下载更新" => {
            i18n::tr(language, "准备下载更新", "Preparing update", "更新準備中").to_string()
        }
        "正在准备从 GitHub Release 下载更新。" => i18n::tr(
            language,
            "正在准备从 GitHub Release 下载更新。",
            "Preparing to download the update from GitHub Release.",
            "GitHub Release から更新をダウンロードする準備をしています。",
        )
        .to_string(),
        "下载更新" => i18n::tr(
            language,
            "下载更新",
            "Downloading update",
            "更新をダウンロード中",
        )
        .to_string(),
        "校验更新文件" => i18n::tr(
            language,
            "校验更新文件",
            "Verifying update",
            "更新ファイルを検証中",
        )
        .to_string(),
        "正在校验 Windows 可执行文件。" => i18n::tr(
            language,
            "正在校验 Windows 可执行文件。",
            "Verifying the Windows executable.",
            "Windows 実行ファイルを検証しています。",
        )
        .to_string(),
        "保存更新文件" => i18n::tr(
            language,
            "保存更新文件",
            "Saving update",
            "更新ファイルを保存中",
        )
        .to_string(),
        "准备替换更新" => i18n::tr(
            language,
            "准备替换更新",
            "Preparing replacement",
            "置き換え準備中",
        )
        .to_string(),
        "正在生成自动替换脚本。" => i18n::tr(
            language,
            "正在生成自动替换脚本。",
            "Generating the automatic replacement script.",
            "自動置き換えスクリプトを生成しています。",
        )
        .to_string(),
        "准备重启" => i18n::tr(
            language,
            "准备重启",
            "Restarting",
            "再起動準備中",
        )
        .to_string(),
        "工具箱将关闭，更新助手会替换当前文件并重新启动。" => i18n::tr(
            language,
            "工具箱将关闭，更新助手会替换当前文件并重新启动。",
            "The toolbox will close; the update helper will replace this executable and restart it.",
            "ツールボックスを閉じ、更新ヘルパーが現在の実行ファイルを置き換えて再起動します。",
        )
        .to_string(),
        "下载失败" => {
            i18n::tr(language, "下载失败", "Download failed", "ダウンロード失敗").to_string()
        }
        value if value.starts_with("使用下载代理：") => {
            let proxy = value.trim_start_matches("使用下载代理：");
            let proxy = if proxy == "直连 GitHub" {
                i18n::tr(language, "直连 GitHub", "Direct GitHub", "GitHub 直接接続")
            } else {
                proxy
            };
            format!(
                "{}{}",
                i18n::tr(
                    language,
                    "使用下载代理：",
                    "Download proxy: ",
                    "ダウンロードプロキシ: "
                ),
                proxy
            )
        }
        value if value.starts_with("运行目录不可写，改存到下载缓存：") => format!(
            "{}{}",
            i18n::tr(
                language,
                "运行目录不可写，改存到下载缓存：",
                "Runtime folder is not writable. Saving to cache: ",
                "実行フォルダーに書き込めないためキャッシュに保存します: "
            ),
            value.trim_start_matches("运行目录不可写，改存到下载缓存：")
        ),
        value if value.starts_with("更新文件：已下载 ") => value.replace(
            "更新文件：已下载 ",
            i18n::tr(
                language,
                "更新文件：已下载 ",
                "Update file: downloaded ",
                "更新ファイル: ダウンロード済み ",
            ),
        ),
        value if value.starts_with("保存更新文件：") => format!(
            "{}{}",
            i18n::tr(
                language,
                "保存更新文件：",
                "Saving update file: ",
                "更新ファイルを保存: "
            ),
            value.trim_start_matches("保存更新文件：")
        ),
        "准备安装" => i18n::tr(
            language,
            "准备安装",
            "Preparing install",
            "インストール準備中",
        )
        .to_string(),
        "验证内嵌载荷和目标目录。" => i18n::tr(
            language,
            "验证内嵌载荷和目标目录。",
            "Verifying embedded payload and target path.",
            "内蔵ペイロードと対象パスを確認しています。",
        )
        .to_string(),
        "检查游戏 Win64 目录。" => i18n::tr(
            language,
            "检查游戏 Win64 目录。",
            "Checking the game Win64 folder.",
            "ゲームの Win64 フォルダーを確認しています。",
        )
        .to_string(),
        "下载 ProjectRebound" => i18n::tr(
            language,
            "下载 ProjectRebound",
            "Downloading ProjectRebound",
            "ProjectRebound をダウンロード中",
        )
        .to_string(),
        "校验 ProjectRebound" => i18n::tr(
            language,
            "校验 ProjectRebound",
            "Verifying ProjectRebound",
            "ProjectRebound を検証中",
        )
        .to_string(),
        "检查在线包内的 Payload.dll 和包装器。" => i18n::tr(
            language,
            "检查在线包内的 Payload.dll 和包装器。",
            "Checking Payload.dll and the wrapper in the online package.",
            "オンラインパッケージ内の Payload.dll とラッパーを確認しています。",
        )
        .to_string(),
        "准备登录服务器" => i18n::tr(
            language,
            "准备登录服务器",
            "Preparing login server",
            "ログインサーバー準備中",
        )
        .to_string(),
        "从 GitHub 下载 BoundaryMetaServer。" => i18n::tr(
            language,
            "从 GitHub 下载 BoundaryMetaServer。",
            "Downloading BoundaryMetaServer from GitHub.",
            "GitHub から BoundaryMetaServer をダウンロードしています。",
        )
        .to_string(),
        "下载 BoundaryMetaServer" => i18n::tr(
            language,
            "下载 BoundaryMetaServer",
            "Downloading BoundaryMetaServer",
            "BoundaryMetaServer をダウンロード中",
        )
        .to_string(),
        "登录服务器已准备" => i18n::tr(
            language,
            "登录服务器已准备",
            "Login server ready",
            "ログインサーバー準備完了",
        )
        .to_string(),
        "准备 Node.js" => i18n::tr(
            language,
            "准备 Node.js",
            "Preparing Node.js",
            "Node.js 準備中",
        )
        .to_string(),
        "查询最新 LTS 运行时。" => i18n::tr(
            language,
            "查询最新 LTS 运行时。",
            "Querying the latest LTS runtime.",
            "最新の LTS ランタイムを確認しています。",
        )
        .to_string(),
        "下载 Node.js" => i18n::tr(
            language,
            "下载 Node.js",
            "Downloading Node.js",
            "Node.js をダウンロード中",
        )
        .to_string(),
        "Node.js 已准备" => i18n::tr(
            language,
            "Node.js 已准备",
            "Node.js ready",
            "Node.js 準備完了",
        )
        .to_string(),
        "准备写入文件" => i18n::tr(
            language,
            "准备写入文件",
            "Preparing file writes",
            "ファイル書き込み準備中",
        )
        .to_string(),
        "关闭相关运行进程并清理旧配置。" => i18n::tr(
            language,
            "关闭相关运行进程并清理旧配置。",
            "Stopping related runtime processes and cleaning old configuration.",
            "関連実行プロセスを停止し、古い設定をクリーンアップしています。",
        )
        .to_string(),
        "写入安装文件" => i18n::tr(
            language,
            "写入安装文件",
            "Writing install files",
            "インストールファイルを書き込み中",
        )
        .to_string(),
        "安装登录服务器依赖" => i18n::tr(
            language,
            "安装登录服务器依赖",
            "Installing login server dependencies",
            "ログインサーバー依存関係をインストール中",
        )
        .to_string(),
        "执行 npm ci --omit=dev --ignore-scripts，使用国内 npm 源下载依赖。" => {
            i18n::tr(
                language,
                "执行 npm ci --omit=dev --ignore-scripts，使用国内 npm 源下载依赖。",
                "Running npm ci --omit=dev --ignore-scripts with the China npm mirror.",
                "国内 npm ミラーを使って npm ci --omit=dev --ignore-scripts を実行しています。",
            )
            .to_string()
        }
        "写入安装记录" => i18n::tr(
            language,
            "写入安装记录",
            "Writing install record",
            "インストール記録を書き込み中",
        )
        .to_string(),
        "生成 state.json 和安装标记。" => i18n::tr(
            language,
            "生成 state.json 和安装标记。",
            "Generating state.json and install markers.",
            "state.json とインストールマーカーを生成しています。",
        )
        .to_string(),
        "安装完成" => {
            i18n::tr(language, "安装完成", "Install complete", "インストール完了").to_string()
        }
        "社区服文件已写入目标目录。" => i18n::tr(
            language,
            "社区服文件已写入目标目录。",
            "Community server files were written to the target folder.",
            "コミュニティサーバーファイルを対象フォルダーに書き込みました。",
        )
        .to_string(),
        value if value.starts_with("使用缓存：") => format!(
            "{}{}",
            i18n::tr(language, "使用缓存：", "Using cache: ", "キャッシュ使用: "),
            value.trim_start_matches("使用缓存：")
        ),
        value if value.starts_with("下载完成：") => format!(
            "{}{}",
            i18n::tr(
                language,
                "下载完成：",
                "Download complete: ",
                "ダウンロード完了: "
            ),
            value.trim_start_matches("下载完成：")
        ),
        value if value.contains("：已下载 ") => value
            .replace(
                "Windows 运行时 zip",
                i18n::tr(
                    language,
                    "Windows 运行时 zip",
                    "Windows runtime zip",
                    "Windows ランタイム zip",
                ),
            )
            .replace(
                "：已下载 ",
                i18n::tr(
                    language,
                    "：已下载 ",
                    ": downloaded ",
                    ": ダウンロード済み ",
                ),
            ),
        value => value.to_string(),
    }
}

fn localize_dialog_text(text: &str, language: i32) -> String {
    text.lines()
        .map(|line| localize_dialog_line(line, language))
        .collect::<Vec<_>>()
        .join("\n")
}

fn localize_dialog_line(line: &str, language: i32) -> String {
    match line {
        "安装完成。" => i18n::tr(language, "安装完成。", "Install complete.", "インストール完了。").to_string(),
        "Maple Mono NF CN 自动安装完成。" => i18n::tr(
            language,
            "Maple Mono NF CN 自动安装完成。",
            "Maple Mono NF CN was installed automatically.",
            "Maple Mono NF CN の自動インストールが完了しました。",
        )
        .to_string(),
        value if value.starts_with("已安装/更新 ") && value.ends_with(" 个字体文件。") => value
            .replace(
                "已安装/更新 ",
                i18n::tr(
                    language,
                    "已安装/更新 ",
                    "Installed/updated ",
                    "インストール/更新済み ",
                ),
            )
            .replace(
                " 个字体文件。",
                i18n::tr(language, " 个字体文件。", " font files.", " 個のフォントファイル。"),
            ),
        value if value.starts_with("来源：") => format!(
            "{}{}",
            i18n::tr(language, "来源：", "Source: ", "ソース: "),
            value.trim_start_matches("来源：")
        ),
        "如果当前界面没有立刻切换字体，请重启工具箱。" => i18n::tr(
            language,
            "如果当前界面没有立刻切换字体，请重启工具箱。",
            "If the UI font does not switch immediately, restart the toolbox.",
            "UI フォントがすぐに切り替わらない場合は、ツールボックスを再起動してください。",
        )
        .to_string(),
        "Payload.dll 和 ProjectReboundServerWrapper.exe 已从在线 Nightly Release 更新。" => i18n::tr(
            language,
            "Payload.dll 和 ProjectReboundServerWrapper.exe 已从在线 Nightly Release 更新。",
            "Payload.dll and ProjectReboundServerWrapper.exe were updated from the online Nightly Release.",
            "Payload.dll と ProjectReboundServerWrapper.exe をオンライン Nightly Release から更新しました。",
        )
        .to_string(),
        "PVP 启动完成。" => i18n::tr(language, "PVP 启动完成。", "PVP launch complete.", "PVP 起動完了。").to_string(),
        "PVE 启动完成。" => i18n::tr(language, "PVE 启动完成。", "PVE launch complete.", "PVE 起動完了。").to_string(),
        "卸载完成。" => i18n::tr(language, "卸载完成。", "Uninstall complete.", "アンインストール完了。").to_string(),
        value if value.starts_with("BoundaryMetaServer 已从 GitHub 在线安装：") => format!(
            "{}{}",
            i18n::tr(
                language,
                "BoundaryMetaServer 已从 GitHub 在线安装：",
                "BoundaryMetaServer was installed online from GitHub: ",
                "BoundaryMetaServer を GitHub からオンラインインストールしました: "
            ),
            value
                .trim_start_matches("BoundaryMetaServer 已从 GitHub 在线安装：")
                .trim_end_matches('。')
        ),
        value if value.starts_with("Node.js 运行时已在线安装：") => format!(
            "{}{}",
            i18n::tr(
                language,
                "Node.js 运行时已在线安装：",
                "Node.js runtime was installed online: ",
                "Node.js ランタイムをオンラインインストールしました: "
            ),
            value
                .trim_start_matches("Node.js 运行时已在线安装：")
                .trim_end_matches('。')
        ),
        value if value.starts_with("并已清理冲突配置：") => format!(
            "{}{}",
            i18n::tr(
                language,
                "并已清理冲突配置：",
                "Also cleaned conflicting config: ",
                "競合する設定もクリーンアップしました: "
            ),
            value.trim_start_matches("并已清理冲突配置：")
        ),
        value => value.to_string(),
    }
}
