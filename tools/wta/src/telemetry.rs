// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// ETW TraceLogging provider for the WTA (Windows Terminal Agent) process.
//
// This module emits ETW events that are byte-identical to the C++
// TraceLoggingWrite() calls in src/cascadia/TerminalApp/. WTA registers the
// SAME provider GUID as the C++ side (see src/cascadia/TerminalApp/init.cpp,
// g_hTerminalAgentProvider). Listeners therefore see a single merged event
// stream for the entire fork, joinable by SessionId.
//
// Events emitted from this module:
//   - AgentPromptSent       (WTA dispatches a prompt over ACP)
//   - AgentResponseReceived (ACP returns first token / completes)
//   - ErrorDetected         (classify_wt_event positively classifies an error)
//   - ErrorFixResolved      (next command's exit code is 0 after a fix attempt)
//
// All events follow the same conventions as the C++ side:
//   - TraceLoggingDescription equivalent: passed as the event's metadata comment
//   - Keyword: MICROSOFT_KEYWORD_MEASURES (stub = 0 in OSS; real value in MS-internal build)
//   - PartA_PrivTags: PDT_ProductAndServiceUsage (stub = 0 in OSS)

use tracelogging as tlg;

// Compliance stubs — these mirror the values defined in
// `dep/telemetry/ProjectTelemetry.h` on the C++ side. In the public OSS build
// they are all zero. Microsoft-internal builds replace them with real values.
#[allow(dead_code)]
pub const MICROSOFT_KEYWORD_MEASURES: u64 = 0x0;
#[allow(dead_code)]
pub const MICROSOFT_KEYWORD_TELEMETRY: u64 = 0x0;
pub const PDT_PRODUCT_AND_SERVICE_USAGE: u64 = 0x0;
pub const PDT_PRODUCT_AND_SERVICE_PERFORMANCE: u64 = 0x0;

// Provider definition.
//
// Provider name MUST match the C++ side (`Microsoft.Windows.Terminal.Agent`,
// see src/cascadia/TerminalApp/init.cpp). The same applies to the GUID:
// {c2cc7e3b-9d5f-4a2e-b8a4-1f3e5d7c9b6a}.
//
// `group_id` is the Microsoft Telemetry option group, equivalent to the C++
// TraceLoggingOptionMicrosoftTelemetry() macro
// (group GUID: 9aa7a361-583f-4c09-b1f1-cea1ef5863b0).
tlg::define_provider!(
    AGENT_PROVIDER,
    "Microsoft.Windows.Terminal.Agent",
    id("c2cc7e3b-9d5f-4a2e-b8a4-1f3e5d7c9b6a"),
    group_id("9aa7a361-583f-4c09-b1f1-cea1ef5863b0")
);

/// Register the ETW provider. Call once during process startup, BEFORE any
/// log_* function below. Safe to call only once; subsequent calls are no-ops.
///
/// # Safety
/// `TraceLoggingRegister`-style APIs are inherently per-process. The
/// `tracelogging` crate marks this `unsafe` for that reason. We call it from
/// `main()` exactly once, which satisfies the contract.
pub fn register() {
    // SAFETY: called once during startup; matches C++-side single-registration pattern.
    unsafe {
        AGENT_PROVIDER.register();
    }
}

/// Unregister the ETW provider. Optional; the OS reclaims the registration
/// on process exit. We provide it for symmetry with the C++ side's
/// DLL_PROCESS_DETACH path.
#[allow(dead_code)]
pub fn unregister() {
    AGENT_PROVIDER.unregister();
}

/// Emitted when WTA dispatches a prompt over the ACP stream to an agent.
///
/// This is the WTA-side counterpart of `AgentPromptSent` on the C++ side
/// (which fires when the `?<prompt>` command-palette delegation route enters
/// the Terminal). Together they cover the two prompt-entry routes.
pub fn log_agent_prompt_sent(
    session_id: &str,
    prompt_byte_len: u32,
    is_autofix: bool,
    template_kind: &str,
) {
    let is_autofix_i32: i32 = if is_autofix { 1 } else { 0 };
    tlg::write_event!(
        AGENT_PROVIDER,
        "AgentPromptSent",
        level(Verbose),
        keyword(MICROSOFT_KEYWORD_MEASURES),
        str8("SessionId", session_id),
        u32("PromptLengthBytes", &prompt_byte_len),
        bool32("IsAutofix", &is_autofix_i32),
        str8("TemplateKind", template_kind),
        str8("Route", "AcpDispatch"),
        u64("PartA_PrivTags", &PDT_PRODUCT_AND_SERVICE_USAGE),
    );
}

/// Emitted when the agent's first text chunk arrives back over ACP.
/// `first_token_latency_ms` is wall-clock from prompt dispatch to first token.
pub fn log_agent_response_first_token(
    session_id: &str,
    first_token_latency_ms: f64,
    chunk_byte_len: u32,
) {
    tlg::write_event!(
        AGENT_PROVIDER,
        "AgentResponseReceived",
        level(Verbose),
        keyword(MICROSOFT_KEYWORD_MEASURES),
        str8("SessionId", session_id),
        str8("Phase", "FirstToken"),
        f64("FirstTokenLatencyMs", &first_token_latency_ms),
        u32("ChunkLengthBytes", &chunk_byte_len),
        u64("PartA_PrivTags", &PDT_PRODUCT_AND_SERVICE_PERFORMANCE),
    );
}

/// Emitted when the agent finishes responding (prompt request completes).
/// `total_duration_ms` is wall-clock from prompt dispatch to completion.
/// `total_response_bytes` is the aggregate byte length of all chunks.
pub fn log_agent_response_complete(
    session_id: &str,
    total_duration_ms: f64,
    total_response_bytes: u64,
    success: bool,
) {
    let success_i32: i32 = if success { 1 } else { 0 };
    tlg::write_event!(
        AGENT_PROVIDER,
        "AgentResponseReceived",
        level(Verbose),
        keyword(MICROSOFT_KEYWORD_MEASURES),
        str8("SessionId", session_id),
        str8("Phase", "Complete"),
        f64("TotalDurationMs", &total_duration_ms),
        u64("TotalResponseBytes", &total_response_bytes),
        bool32("Success", &success_i32),
        u64("PartA_PrivTags", &PDT_PRODUCT_AND_SERVICE_PERFORMANCE),
    );
}

/// Emitted when the WTA event classifier positively identifies an error in
/// a pane (e.g., connection failed, process exited with non-zero code).
pub fn log_error_detected(severity: &str, method: &str, pane_id: &str) {
    tlg::write_event!(
        AGENT_PROVIDER,
        "ErrorDetected",
        level(Verbose),
        keyword(MICROSOFT_KEYWORD_MEASURES),
        str8("Severity", severity),
        str8("Method", method),
        str8("PaneId", pane_id),
        u64("PartA_PrivTags", &PDT_PRODUCT_AND_SERVICE_USAGE),
    );
}

/// Emitted when the next command after an attempted fix succeeds (exit 0)
/// in the same pane where autofix was armed. `time_since_fix_ms` is
/// wall-clock from arming the fix to observing the successful exit.
pub fn log_error_fix_resolved(pane_id: &str, time_since_fix_ms: f64) {
    tlg::write_event!(
        AGENT_PROVIDER,
        "ErrorFixResolved",
        level(Verbose),
        keyword(MICROSOFT_KEYWORD_MEASURES),
        str8("PaneId", pane_id),
        f64("TimeSinceFixMs", &time_since_fix_ms),
        u64("PartA_PrivTags", &PDT_PRODUCT_AND_SERVICE_USAGE),
    );
}
