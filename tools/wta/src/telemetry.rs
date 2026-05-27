// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// ETW TraceLogging provider for the WTA (Windows Terminal Agent) process.
//
// This module registers the SAME ETW provider as the C++ Windows Terminal
// side (`Microsoft.Windows.Terminal.App`, GUID
// `{24a1622f-7da7-5c77-3303-d850bd1ab2ed}`, registered by
// `g_hTerminalAppProvider` in `src/cascadia/TerminalApp/init.cpp`). WTA and
// the C++ side therefore emit into a single merged ETW provider stream, so
// listeners (xperf/wpa/UTC) see one unified view of the fork.
//
// Note: cross-process correlation by `SessionId` is intentionally NOT
// provided here. The `SessionId` field on WTA events identifies the ACP
// session (agent-pane backend connection); the C++-side events under this
// provider (e.g. `CommandPaletteDispatchedAgentPrompt`) emit different
// fields and do not carry a matching ACP session id. Treat the two
// processes' events as a merged stream from one provider, but join across
// processes only via fields explicitly shared (e.g. `PaneId`).
//
// Events emitted from this module:
//   - AgentPromptSent          (WTA dispatches a prompt over ACP)
//   - AgentResponseFirstToken  (ACP returns the first text chunk)
//   - AgentResponseComplete    (ACP prompt request completes)
//   - ErrorDetected            (classify_wt_event positively classifies an error)
//   - ErrorFixResolved         (next command's exit code is 0 after a fix attempt)
//
// Conventions:
//   - TraceLoggingDescription equivalent: passed as the event's metadata comment
//   - Keyword: MICROSOFT_KEYWORD_MEASURES (stub = 0 in OSS; real value in MS-internal build)
//   - PartA_PrivTags: PDT_ProductAndServiceUsage (stub = 0 in OSS)
//   - Level: `Verbose`, matching `TraceLoggingWrite`'s C++ default (the C++
//     events under this provider also use the implicit Verbose level).

use tracelogging as tlg;

// Compliance stubs — these mirror the values defined in
// `dep/telemetry/ProjectTelemetry.h` used by Microsoft-internal builds. In the
// public OSS build they are all zero; Microsoft-internal builds replace them
// with real values.
#[allow(dead_code)]
pub const MICROSOFT_KEYWORD_MEASURES: u64 = 0x0;
#[allow(dead_code)]
pub const MICROSOFT_KEYWORD_TELEMETRY: u64 = 0x0;
pub const PDT_PRODUCT_AND_SERVICE_USAGE: u64 = 0x0;
pub const PDT_PRODUCT_AND_SERVICE_PERFORMANCE: u64 = 0x0;

// Provider definition.
//
// Provider name and GUID match the C++ side
// (`Microsoft.Windows.Terminal.App`, `g_hTerminalAppProvider` in
// `src/cascadia/TerminalApp/init.cpp`). Both processes therefore emit into
// the same ETW provider stream and listeners get a unified view of the fork.
//
// `group_id` is the Microsoft Telemetry option group, equivalent to the C++
// TraceLoggingOptionMicrosoftTelemetry() macro
// (group GUID: 9aa7a361-583f-4c09-b1f1-cea1ef5863b0).
tlg::define_provider!(
    AGENT_PROVIDER,
    "Microsoft.Windows.Terminal.App",
    id("24a1622f-7da7-5c77-3303-d850bd1ab2ed"),
    group_id("9aa7a361-583f-4c09-b1f1-cea1ef5863b0")
);

/// Register the ETW provider. Safe to call multiple times — the underlying
/// `TraceLoggingRegister`-style API is guarded by a `Once`, so only the
/// first invocation actually performs the (unsafe) registration. Subsequent
/// calls are no-ops, which keeps tests and re-entrant startup paths safe.
///
/// # Safety
/// `TraceLoggingRegister`-style APIs are inherently per-process. The
/// `tracelogging` crate marks `register()` `unsafe` for that reason. The
/// `Once` below guarantees the unsafe call runs exactly once.
pub fn register() {
    static REGISTER_ONCE: std::sync::Once = std::sync::Once::new();
    REGISTER_ONCE.call_once(|| {
        // SAFETY: the surrounding `Once` guarantees this runs exactly once
        // per process; `tracelogging`'s contract is satisfied.
        unsafe {
            AGENT_PROVIDER.register();
        }
    });
}

/// Unregister the ETW provider. Optional; the OS reclaims the registration
/// on process exit. No-op if `register()` was never called.
#[allow(dead_code)]
pub fn unregister() {
    AGENT_PROVIDER.unregister();
}

/// Emitted when WTA dispatches a prompt over the ACP stream to an agent.
///
/// Covers the agent-pane prompt-entry route. The C++ side emits its own
/// related event for the `?<prompt>` command-palette delegation route
/// (`CommandPaletteDispatchedAgentPrompt` in
/// `src/cascadia/TerminalApp/CommandPalette.cpp`, under the same provider).
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
///
/// Uses a distinct event name (`AgentResponseFirstToken`) so the field
/// schema is unambiguous in downstream decoders — keeping a single
/// `AgentResponseReceived` name across the first-token and complete cases
/// would yield two metadata definitions for the same event name in ETW
/// (the schemas differ), which complicates query/decode.
pub fn log_agent_response_first_token(
    session_id: &str,
    first_token_latency_ms: f64,
    chunk_byte_len: u32,
) {
    tlg::write_event!(
        AGENT_PROVIDER,
        "AgentResponseFirstToken",
        level(Verbose),
        keyword(MICROSOFT_KEYWORD_MEASURES),
        str8("SessionId", session_id),
        f64("FirstTokenLatencyMs", &first_token_latency_ms),
        u32("ChunkLengthBytes", &chunk_byte_len),
        u64("PartA_PrivTags", &PDT_PRODUCT_AND_SERVICE_PERFORMANCE),
    );
}

/// Emitted when the agent finishes responding (prompt request completes).
/// `total_duration_ms` is wall-clock from prompt dispatch to completion.
/// `raw_stdout_bytes_after_prompt` is the raw byte count read from the
/// agent CLI's stdout after the prompt was dispatched. This includes the
/// JSON-RPC framing / tool-call payloads, not just the user-visible text
/// chunks — it is a transport-level volume metric, not a measure of the
/// final answer length. The ETW field name (`TotalResponseBytes`) is
/// preserved for downstream compatibility.
///
/// Uses a distinct event name (`AgentResponseComplete`) — see the note on
/// `log_agent_response_first_token` for why this is split into two events.
pub fn log_agent_response_complete(
    session_id: &str,
    total_duration_ms: f64,
    raw_stdout_bytes_after_prompt: u64,
    success: bool,
) {
    let success_i32: i32 = if success { 1 } else { 0 };
    tlg::write_event!(
        AGENT_PROVIDER,
        "AgentResponseComplete",
        level(Verbose),
        keyword(MICROSOFT_KEYWORD_MEASURES),
        str8("SessionId", session_id),
        f64("TotalDurationMs", &total_duration_ms),
        u64("TotalResponseBytes", &raw_stdout_bytes_after_prompt),
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
