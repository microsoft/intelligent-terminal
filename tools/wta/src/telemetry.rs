// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

//! ETW telemetry helpers for WTA.
//!
//! Provides thin convenience wrappers around the generated
//! [`WtaTelemetryEvents`] provider so call-sites don't need to pass
//! boilerplate fields (`PartA_PrivTags`, agent id) manually.
//!
//! # Usage
//!
//! ```ignore
//! // At startup:
//! telemetry::init();
//!
//! // Once agent identity is known:
//! telemetry::set_agent_id("copilot");
//!
//! // At each instrumented site:
//! telemetry::agent_prompt_sent(is_autofix);
//! ```

// Include the build-time generated trait impl (GUID-injected copy of
// telemetry_template.rs).
#[allow(non_snake_case)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/telemetry_generated.rs"));
}
use generated::WtaTelemetryEvents;

use std::sync::{Mutex, OnceLock};

/// OSS placeholder — internal builds compile in the real value via
/// build-system injection, mirroring `PDT_ProductAndServiceUsage` on the
/// C++ side (`dep/telemetry/ProjectTelemetry.h`).
const PDT_PRODUCT_AND_SERVICE_USAGE: u64 = 0;

/// Singleton provider instance.
static PROVIDER: OnceLock<WtaTelemetryEvents> = OnceLock::new();

/// Current agent identifier (e.g. `"copilot"`, `"gemini"`).  Set once
/// at connection time by the ACP client; read by every event wrapper.
static AGENT_ID: Mutex<String> = Mutex::new(String::new());

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Register the ETW provider.  Safe to call more than once (idempotent via
/// `OnceLock`).
pub fn init() {
    let _ = PROVIDER.get_or_init(WtaTelemetryEvents::new);
}

// ---------------------------------------------------------------------------
// Agent identity
// ---------------------------------------------------------------------------

/// Record the current agent id for subsequent telemetry events.
pub fn set_agent_id(id: &str) {
    if let Ok(mut guard) = AGENT_ID.lock() {
        guard.clear();
        guard.push_str(id);
    }
}

fn current_agent_id() -> String {
    AGENT_ID
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Convenience wrappers — one per event
// ---------------------------------------------------------------------------

fn provider() -> &'static WtaTelemetryEvents {
    PROVIDER.get_or_init(WtaTelemetryEvents::new)
}

/// A user prompt was dispatched to the agent.
pub fn agent_prompt_sent(is_autofix: bool) {
    let id = current_agent_id();
    provider().agent_prompt_sent(None, &id, is_autofix, PDT_PRODUCT_AND_SERVICE_USAGE);
}

/// The agent's response completed (success **or** error).
pub fn agent_response_received(success: bool, duration_ms: u64) {
    let id = current_agent_id();
    provider().agent_response_received(None, &id, success, duration_ms, PDT_PRODUCT_AND_SERVICE_USAGE);
}

/// The user executed a recommended action from the agent.
pub fn agent_response_action(action_type: &str, is_autofix: bool) {
    let id = current_agent_id();
    provider().agent_response_action(None, &id, action_type, is_autofix, PDT_PRODUCT_AND_SERVICE_USAGE);
}

/// An autofix cycle resolved.
pub fn error_fix_resolved(resolution: &str) {
    let id = current_agent_id();
    provider().error_fix_resolved(None, &id, resolution, PDT_PRODUCT_AND_SERVICE_USAGE);
}
