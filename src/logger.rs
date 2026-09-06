use std::{fs, sync::OnceLock};
use tracing::info;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

static LOG_GUARD: OnceLock<WorkerGuard> = OnceLock::new();

pub fn init_logger(log_file: &str) {
    let log_dir = crate::paths::get().log_dir();
    fs::create_dir_all(log_dir).ok();
    let file_appender = RollingFileAppender::new(Rotation::DAILY, log_dir, log_file);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    let _ = LOG_GUARD.set(guard);

    let file_layer = fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_thread_ids(false)
        .with_thread_names(false);

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(file_layer)
        .init();

    info!("ctlyrics started");
}
