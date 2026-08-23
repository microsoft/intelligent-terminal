//! Shared renderer test infrastructure.

use rstest::fixture;
use tracing::level_filters::LevelFilter;
use tracing::subscriber;
pub use tracing::subscriber::DefaultGuard;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::fmt::time::Uptime;

#[fixture]
pub fn with_tracing() -> DefaultGuard {
    let subscriber = tracing_subscriber::fmt()
        .with_test_writer()
        .with_timer(Uptime::default())
        .with_max_level(LevelFilter::TRACE)
        .with_span_events(FmtSpan::ENTER)
        .finish();
    subscriber::set_default(subscriber)
}
