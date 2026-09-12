//! Opt-in, buffered startup checkpoints. No payloads or per-checkpoint log I/O.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub(crate) struct StartupTiming {
    scope: &'static str,
    recording: Option<Recording>,
}

struct Recording {
    id: u64,
    start: Instant,
    start_unix_us: u128,
    checkpoints: Vec<(&'static str, u128)>,
}

impl StartupTiming {
    pub(crate) fn new(scope: &'static str) -> Self {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        Self::with_enabled(
            scope,
            *ENABLED.get_or_init(|| std::env::var("WTA_STARTUP_TIMING").as_deref() == Ok("1")),
        )
    }

    fn with_enabled(scope: &'static str, enabled: bool) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Self {
            scope,
            recording: enabled.then(|| Recording {
                id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
                start: Instant::now(),
                start_unix_us: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|duration| duration.as_micros())
                    .unwrap_or_default(),
                checkpoints: Vec::with_capacity(32),
            }),
        }
    }

    pub(crate) fn mark(&mut self, phase: &'static str) {
        if let Some(recording) = &mut self.recording {
            recording
                .checkpoints
                .push((phase, recording.start.elapsed().as_micros()));
        }
    }
}

impl Drop for StartupTiming {
    fn drop(&mut self) {
        if let Some(recording) = &self.recording {
            tracing::info!(
                target: "startup_timing",
                pid = std::process::id(),
                span = recording.id,
                scope = self.scope,
                start_unix_us = %recording.start_unix_us,
                elapsed_us = %recording.start.elapsed().as_micros(),
                checkpoints = ?recording.checkpoints,
                "startup timing scope ended"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_timing_does_not_allocate_checkpoints() {
        let mut timing = StartupTiming::with_enabled("test", false);
        timing.mark("unused");
        assert!(timing.recording.is_none());
    }

    #[test]
    fn checkpoints_keep_order_and_monotonic_offsets() {
        let mut timing = StartupTiming::with_enabled("test", true);
        timing.mark("first");
        timing.mark("second");
        let recording = timing.recording.as_ref().unwrap();
        assert_eq!(recording.checkpoints.len(), 2);
        assert_eq!(recording.checkpoints[0].0, "first");
        assert_eq!(recording.checkpoints[1].0, "second");
        assert!(recording.checkpoints[0].1 <= recording.checkpoints[1].1);
    }
}
