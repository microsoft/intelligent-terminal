use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Returns the default `EnvFilter` directive to use when neither `WTA_LOG` nor
/// `RUST_LOG` is set.
///
/// `debug_assertions` is passed in (rather than read from `cfg!`) so that the
/// release-build branch can be unit-tested even when the test binary itself is
/// compiled in debug mode.
pub(crate) fn default_filter_directive(debug_assertions: bool) -> &'static str {
    if debug_assertions {
        // Verbose for developers iterating on the code.
        "debug"
    } else {
        // Quiet in shipping release binaries. Users troubleshooting can opt in
        // by setting `WTA_LOG=info|debug|trace` or `RUST_LOG=...`.
        "warn"
    }
}

pub fn init(process: &str) -> WorkerGuard {
    let log_dir = crate::runtime_paths::intelligent_terminal_root()
        .map(|r| r.join("logs"))
        .unwrap_or_else(|| std::env::temp_dir().join("IntelligentTerminal").join("logs"));
    let _ = std::fs::create_dir_all(&log_dir);

    let file_name = format!("wta-{process}.log");
    let appender = tracing_appender::rolling::never(&log_dir, &file_name);
    let (non_blocking, guard) = tracing_appender::non_blocking(appender);

    let default_level = default_filter_directive(cfg!(debug_assertions));

    let filter = EnvFilter::try_from_env("WTA_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new(default_level));

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

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::filter::LevelFilter;

    #[test]
    fn debug_build_default_is_debug() {
        assert_eq!(default_filter_directive(true), "debug");
    }

    #[test]
    fn release_build_default_is_warn() {
        assert_eq!(default_filter_directive(false), "warn");
    }

    #[test]
    fn release_default_filter_rejects_info_and_below() {
        // The EnvFilter built from the release default must NOT enable info,
        // debug, or trace events — only warn and error.
        let filter = EnvFilter::new(default_filter_directive(false));
        assert_eq!(filter.max_level_hint(), Some(LevelFilter::WARN));
    }

    #[test]
    fn debug_default_filter_enables_debug() {
        let filter = EnvFilter::new(default_filter_directive(true));
        assert_eq!(filter.max_level_hint(), Some(LevelFilter::DEBUG));
    }
}
