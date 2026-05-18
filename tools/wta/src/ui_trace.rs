use std::time::Duration;

pub fn log(message: &str) {
    tracing::debug!(target: "ui_trace", "{}", message);
}

pub fn log_slow<F>(scope: &str, elapsed: Duration, details: F)
where
    F: FnOnce() -> String,
{
    if elapsed >= Duration::from_millis(75) {
        tracing::debug!(
            target: "ui_trace",
            scope,
            elapsed_ms = elapsed.as_millis(),
            "slow: {}",
            details()
        );
    }
}
