// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

// ETW telemetry event definitions for the Windows Terminal Agent (WTA).
//
// This file is a *template*: `build.rs` copies it to `$OUT_DIR` with the
// placeholder `provider_group_guid` replaced by the real MS telemetry group
// GUID when the `MAGIC_TRACING_GUID` environment variable is set (internal
// builds). OSS builds keep the placeholder, which means the events are
// still emitted but land in an unrouted provider group.
//
// The macro generates a struct `WtaTelemetryEvents` with one method per
// event.  All events carry `PartA_PrivTags` for privacy classification
// and use `keyword = 0x0` (OSS placeholder — internal builds override via
// the crate feature / build-system injection).

use win_etw_macros::trace_logging_provider;

#[trace_logging_provider(
    name = "Microsoft.Windows.Terminal.Wta",
    guid = "ae1d39f0-4cbd-4c6d-b13b-494bf80d07e3",
    provider_group_guid = "ffffffff-ffff-ffff-ffff-ffffffffffff"
)]
pub trait WtaTelemetryEvents {
    /// Emitted when a user prompt is dispatched to the agent.
    #[event(keyword = 0x0)]
    fn agent_prompt_sent(agent_id: &str, is_autofix: bool, PartA_PrivTags: u64);

    /// Emitted when the agent's response completes (success or error).
    #[event(keyword = 0x0)]
    fn agent_response_received(
        agent_id: &str,
        success: bool,
        duration_ms: u64,
        PartA_PrivTags: u64,
    );

    /// Emitted when the user executes a recommended action from the agent.
    #[event(keyword = 0x0)]
    fn agent_response_action(
        agent_id: &str,
        action_type: &str,
        is_autofix: bool,
        PartA_PrivTags: u64,
    );

    /// Emitted when an autofix cycle resolves (fix suggested, explained,
    /// ignored, or applied by the user).
    #[event(keyword = 0x0)]
    fn error_fix_resolved(agent_id: &str, resolution: &str, PartA_PrivTags: u64);
}
