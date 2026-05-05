//! 应用后台轮询线程。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use crossbeam_channel::Sender;

use crate::core::InstallerCore;

use super::AppMessage;

pub(super) fn spawn_port_thread(
    core: Arc<InstallerCore>,
    tx: Sender<AppMessage>,
    stop: Arc<AtomicBool>,
    active_page: Arc<AtomicI32>,
    target: Arc<RwLock<Option<PathBuf>>>,
) {
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            if active_page.load(Ordering::Relaxed) == 2 {
                let target_path = target.read().ok().and_then(|guard| guard.clone());
                if let Ok(rows) = core.port_status_rows_for_target(target_path.as_deref()) {
                    let _ = tx.send(AppMessage::PortRows(rows));
                }
            }
            thread::sleep(Duration::from_secs(2));
        }
    });
}
