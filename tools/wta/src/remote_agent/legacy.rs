//! Isolated ingestion boundary for legacy hook/session summaries.
//!
//! The existing WTA master/helper hook flow intentionally does not depend on
//! this MVP. A later integration can implement this small trait at the hook
//! boundary without coupling its raw events to relay transport or AHP types.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::state::LegacyIngestOutcome;

/// Credential-free summary imported from a legacy hook or session index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LegacySessionSummary {
    pub(crate) source: String,
    pub(crate) external_id: String,
    pub(crate) title: String,
    #[serde(default = "idle_status")]
    pub(crate) status: u32,
    #[serde(default)]
    pub(crate) activity: Option<String>,
}

impl LegacySessionSummary {
    pub(crate) fn new(source: String, external_id: String, title: String) -> Self {
        Self {
            source,
            external_id,
            title,
            status: idle_status(),
            activity: None,
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        const MAX_SOURCE_BYTES: usize = 64;
        const MAX_EXTERNAL_ID_BYTES: usize = 256;
        const MAX_TITLE_BYTES: usize = 512;
        const MAX_ACTIVITY_BYTES: usize = 512;

        anyhow::ensure!(
            !self.source.trim().is_empty(),
            "legacy source must not be empty"
        );
        anyhow::ensure!(
            self.source.len() <= MAX_SOURCE_BYTES,
            "legacy source exceeds {MAX_SOURCE_BYTES} bytes"
        );
        anyhow::ensure!(
            !self.external_id.trim().is_empty(),
            "legacy session key must not be empty"
        );
        anyhow::ensure!(
            self.external_id.len() <= MAX_EXTERNAL_ID_BYTES,
            "legacy session key exceeds {MAX_EXTERNAL_ID_BYTES} bytes"
        );
        anyhow::ensure!(
            !self.title.trim().is_empty(),
            "legacy title must not be empty"
        );
        anyhow::ensure!(
            self.title.len() <= MAX_TITLE_BYTES,
            "legacy title exceeds {MAX_TITLE_BYTES} bytes"
        );
        anyhow::ensure!(
            self.activity
                .as_ref()
                .is_none_or(|activity| activity.len() <= MAX_ACTIVITY_BYTES),
            "legacy activity exceeds {MAX_ACTIVITY_BYTES} bytes"
        );
        Ok(())
    }
}

/// Compatibility seam for future hook/session-summary producers.
pub(crate) trait LegacySessionSummarySink: Send + Sync {
    fn ingest_legacy_summary(&self, summary: LegacySessionSummary) -> Result<LegacyIngestOutcome>;
}

fn idle_status() -> u32 {
    ahp_types::state::SessionStatus::Idle.bits()
}
