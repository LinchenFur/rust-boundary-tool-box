//! 应用后台轮询线程。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crossbeam_channel::Sender;

use crate::core::InstallerCore;

use super::AppMessage;

pub(super) fn spawn_port_thread(
    core: Arc<InstallerCore>,
    tx: Sender<AppMessage>,
    stop: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            if let Ok(rows) = core.port_status_rows() {
                let _ = tx.send(AppMessage::PortRows(rows));
            }
            thread::sleep(Duration::from_secs(2));
        }
    });
}
