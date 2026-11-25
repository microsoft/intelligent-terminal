//! Shared renderer test infrastructure.
//!
//! Tests should make the input, expected behavior, and failure obvious without following helpers.
//! Keep each test linear and focused. Use direct assertions for simple contracts, inline snapshots
//! for readable output, and external snapshots for large output. Parameterize only when the cases
//! share a clear contract; use fixtures when shared setup would otherwise obscure that contract.
//! Prefer concrete Markdown inputs in rendering assertions when this keeps the case easy to read.
//! Keep local bindings for shared outputs, comparisons, or expressions that become hard to follow.

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
