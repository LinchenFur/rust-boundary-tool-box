//! AppController 社区服务器列表刷新。

use super::*;

impl AppController {
    /// 在工作线程中拉取远程社区服列表。
    pub(super) fn start_refresh_servers(&mut self) {
        if self.ui.get_servers_loading() {
            return;
        }

        self.ui.set_servers_loading(true);
        self.ui
            .set_server_status_text("服务器列表：刷新中...".into());
        let tx = self.tx.clone();
        thread::spawn(move || match fetch_servers() {
            Ok(rows) => {
                let _ = tx.send(AppMessage::ServerRows(rows));
            }
            Err(error) => {
                let _ = tx.send(AppMessage::ServerRowsFailed(error.to_string()));
            }
        });
    }
}
