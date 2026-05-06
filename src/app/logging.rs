//! 应用级日志过滤。

use log::{LevelFilter, Log, Metadata, Record};

static LOGGER: AppLogFilter = AppLogFilter;

pub(super) fn install_log_filter() {
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(LevelFilter::Error);
}

struct AppLogFilter;

impl Log for AppLogFilter {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Error
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        if is_icu_provider_noise_target(record.target()) {
            return;
        }
        let message = record.args().to_string();
        if is_expected_icu_segmenter_warning(record.target(), &message) {
            return;
        }
        eprintln!("{}: {}", record.level(), message);
    }

    fn flush(&self) {}
}

fn is_icu_provider_noise_target(target: &str) -> bool {
    target.starts_with("icu_provider")
}

fn is_expected_icu_segmenter_warning(target: &str, message: &str) -> bool {
    target.starts_with("icu_")
        && message.contains("No segmentation model for language")
        && message.contains("ja")
}
