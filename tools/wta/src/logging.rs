use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

pub fn init(process: &str) -> WorkerGuard {
    let log_dir = crate::runtime_paths::intelligent_terminal_root()
        .map(|r| r.join("logs"))
        .unwrap_or_else(|| std::env::temp_dir().join("IntelligentTerminal").join("logs"));
    let _ = std::fs::create_dir_all(&log_dir);

    let file_name = format!("wta-{process}.log");
    let appender = tracing_appender::rolling::never(&log_dir, &file_name);
    let (non_blocking, guard) = tracing_appender::non_blocking(appender);

    let filter = EnvFilter::try_from_env("WTA_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("debug"));

    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_target(true)
                .with_timer(fmt::time::SystemTime),
        )
        .init();

    guard
}
