//! AppController 磁盘扫描弹窗和扫描任务。

use super::*;

impl AppController {
    /// 为全盘扫描打开自定义盘符选择弹窗。
    pub(super) fn open_drive_dialog(&mut self) {
        if self.ui.get_busy() {
            return;
        }
        let drives = self.core.list_available_drives();
        if drives.is_empty() {
            self.show_error_dialog("全盘扫描", "未找到可扫描的盘符。");
            return;
        }

        while self.drive_model.row_count() > 0 {
            let _ = self.drive_model.remove(self.drive_model.row_count() - 1);
        }
        for drive in drives {
            self.drive_model.push(DriveRow {
                label: SharedString::from(drive.display().to_string()),
                checked: true,
            });
        }
        self.ui.set_show_drive_dialog(true);
    }

    /// 用户切换盘符后更新对应行。
    pub(super) fn toggle_drive(&mut self, index: i32, checked: bool) {
        let index = index.max(0) as usize;
        if let Some(mut row) = self.drive_model.row_data(index) {
            row.checked = checked;
            self.drive_model.set_row_data(index, row);
        }
    }

    /// 收集当前勾选的盘符根目录。
    pub(super) fn selected_drives(&self) -> Vec<PathBuf> {
        (0..self.drive_model.row_count())
            .filter_map(|index| self.drive_model.row_data(index))
            .filter(|row| row.checked)
            .map(|row| PathBuf::from(row.label.to_string()))
            .collect()
    }

    /// 在工作线程中启动盘符扫描。
    pub(super) fn start_drive_scan(&mut self) {
        let drives = self.selected_drives();
        if drives.is_empty() {
            self.show_error_dialog("全盘扫描", "请至少选择一个盘符。");
            return;
        }
        if self.ui.get_busy() {
            return;
        }

        self.ui.set_busy(true);
        self.ui.set_status_text("全盘扫描中...".into());
        self.append_log(&format!(
            "[{}] 开始全盘扫描：{}",
            core::now_text(),
            drives
                .iter()
                .map(|drive| drive.display().to_string())
                .collect::<Vec<_>>()
                .join("、")
        ));

        let core = self.core.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = core.scan_drives_for_game(&drives);
            let dialog = result
                .as_ref()
                .map(|path| format!("已通过全盘扫描找到游戏目录：{}", path.display()))
                .unwrap_or_else(|| "在所选盘符中未找到 Boundary 游戏目录。".to_string());
            let _ = tx.send(AppMessage::ScanFinished { result, dialog });
        });
    }
}
