// tools/wta/src/protocol/internal_control.rs
//
// Wire-format types for `_internal.*` control events on the existing
// IProtocolEventCallback channel between Terminal and wta. See
// doc/specs/Multi-window-agent-pane.md → "Control channel" for the
// design.
//
// Terminal → wta (inbound):
//   * _internal.attach_pane   — bind a (tab_id, conpty HANDLE pair) to
//     a new RenderCtx + TabSession.
//   * _internal.detach_pane   — tear down a tab's RenderCtx and end
//     its ACP session.
//   * _internal.resize_pane   — informational; the conpty SIGWINCH
//     already propagates dimensions on its own.
//
// wta → Terminal (outbound, via proxy.SendEvent):
//   * _internal.attach_pane_ack
//   * _internal.detach_pane_ack
//
// These types carry HANDLE values as `u64`. The integer corresponds to
// a HANDLE in wta's address space that Terminal has just produced
// with `DuplicateHandle`. wta uses the value directly with
// `ConptyReader::from_raw_handle` / `ConptyWriter::from_raw_handle`.
//
// Parsing strategy: the inbound dispatcher in app.rs already inspects
// `event.method` before routing. `try_parse_internal` returns
// `Ok(None)` for non-_internal events so callers can pass them
// through to existing handlers unchanged. Malformed _internal events
// surface as `Err(serde_json::Error)`.

use serde::{Deserialize, Serialize};

/// Inbound _internal.* event sent by Terminal to wta via the existing
/// IProtocolEventCallback.OnEvent path.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "method")]
pub enum InboundEvent {
    #[serde(rename = "_internal.attach_pane")]
    AttachPane {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        params: AttachPaneParams,
    },
    #[serde(rename = "_internal.detach_pane")]
    DetachPane {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        params: DetachPaneParams,
    },
    #[serde(rename = "_internal.resize_pane")]
    ResizePane {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        params: ResizePaneParams,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AttachPaneParams {
    pub tab_id: String,
    /// Conpty slave-side read HANDLE, valid in wta's address space
    /// (already `DuplicateHandle`'d by Terminal). wta wraps this with
    /// `ConptyReader::from_raw_handle`.
    pub pty_in: u64,
    /// Conpty slave-side write HANDLE, valid in wta's address space.
    /// wta wraps this with `ConptyWriter::from_raw_handle` and gives
    /// the result to Ratatui's CrosstermBackend.
    pub pty_out: u64,
    /// Initial pty dimensions, as passed to `CreatePseudoConsole` on
    /// the Terminal side. The wta process can't query these from
    /// Crossterm because the conpty's slave is not wta's controlling
    /// tty — they have to come over the wire.
    pub cols: u16,
    pub rows: u16,
    /// Agent CLI to spawn for this pane ("copilot" | "claude" |
    /// "gemini" | ...). wta uses its existing agent_registry to
    /// resolve the actual command line.
    pub agent_id: String,
    /// Working directory passed to the agent CLI.
    pub initial_cwd: String,
    /// View to show on first render ("chat" | "sessions").
    pub initial_view: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct DetachPaneParams {
    pub tab_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ResizePaneParams {
    pub tab_id: String,
    pub rows: u32,
    pub cols: u32,
}

/// Outbound _internal.*_ack event sent by wta to Terminal via
/// IProtocolServer.SendEvent. Carried by all _internal commands that
/// have a meaningful success/failure signal (attach + detach).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "method")]
pub enum OutboundAck {
    #[serde(rename = "_internal.attach_pane_ack")]
    AttachPaneAck {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        params: AckParams,
    },
    #[serde(rename = "_internal.detach_pane_ack")]
    DetachPaneAck {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        params: AckParams,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AckParams {
    pub tab_id: String,
    pub status: AckStatus,
    /// Human-readable error message when `status == Error`. Absent on
    /// success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AckStatus {
    Ok,
    Error,
}

/// Try to interpret `value` as an `_internal.*` control event.
///
/// Returns:
///   * `Ok(Some(_))` when `value.method` starts with `"_internal."`
///     and the payload parses cleanly.
///   * `Ok(None)` when the event has a `method` field that is NOT an
///     `_internal.*` event (or no `method` at all). The caller should
///     fall through to its existing dispatcher for these.
///   * `Err(_)` when the event claims to be `_internal.*` but the
///     payload is malformed (e.g. missing required params, unknown
///     `_internal.*` method).
pub fn try_parse_internal(
    value: &serde_json::Value,
) -> Result<Option<InboundEvent>, serde_json::Error> {
    let method = match value.get("method").and_then(|m| m.as_str()) {
        Some(m) if m.starts_with("_internal.") => m,
        _ => return Ok(None),
    };
    let _ = method;
    serde_json::from_value(value.clone()).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ───────── Inbound parsing ─────────

    #[test]
    fn parses_attach_pane_with_all_fields() {
        let v = json!({
            "method": "_internal.attach_pane",
            "id": "abc-123",
            "params": {
                "tab_id": "T7",
                "pty_in": 968,
                "pty_out": 976,
                "cols": 120,
                "rows": 40,
                "agent_id": "copilot",
                "initial_cwd": "C:\\Users\\test",
                "initial_view": "chat",
            }
        });
        let evt = try_parse_internal(&v).unwrap().unwrap();
        match evt {
            InboundEvent::AttachPane { id, params } => {
                assert_eq!(id.as_deref(), Some("abc-123"));
                assert_eq!(params.tab_id, "T7");
                assert_eq!(params.pty_in, 968);
                assert_eq!(params.pty_out, 976);
                assert_eq!(params.cols, 120);
                assert_eq!(params.rows, 40);
                assert_eq!(params.agent_id, "copilot");
                assert_eq!(params.initial_cwd, "C:\\Users\\test");
                assert_eq!(params.initial_view, "chat");
            }
            other => panic!("expected AttachPane, got {other:?}"),
        }
    }

    #[test]
    fn parses_attach_pane_without_correlation_id() {
        // `id` is optional — Terminal may fire-and-forget for paths
        // where the ack isn't needed.
        let v = json!({
            "method": "_internal.attach_pane",
            "params": {
                "tab_id": "T7",
                "pty_in": 1,
                "pty_out": 2,
                "cols": 80,
                "rows": 24,
                "agent_id": "claude",
                "initial_cwd": "/tmp",
                "initial_view": "chat",
            }
        });
        let evt = try_parse_internal(&v).unwrap().unwrap();
        match evt {
            InboundEvent::AttachPane { id, .. } => assert!(id.is_none()),
            other => panic!("expected AttachPane, got {other:?}"),
        }
    }

    #[test]
    fn parses_detach_pane() {
        let v = json!({
            "method": "_internal.detach_pane",
            "params": { "tab_id": "T7" }
        });
        let evt = try_parse_internal(&v).unwrap().unwrap();
        match evt {
            InboundEvent::DetachPane { params, .. } => {
                assert_eq!(params.tab_id, "T7");
            }
            other => panic!("expected DetachPane, got {other:?}"),
        }
    }

    #[test]
    fn parses_resize_pane() {
        let v = json!({
            "method": "_internal.resize_pane",
            "params": { "tab_id": "T7", "rows": 40, "cols": 120 }
        });
        let evt = try_parse_internal(&v).unwrap().unwrap();
        match evt {
            InboundEvent::ResizePane { params, .. } => {
                assert_eq!(params.rows, 40);
                assert_eq!(params.cols, 120);
            }
            other => panic!("expected ResizePane, got {other:?}"),
        }
    }

    // ───────── Pass-through behavior ─────────

    #[test]
    fn returns_none_for_non_internal_method() {
        // Non-_internal events flow through to wta's existing
        // dispatcher (autofix, tab_changed, etc.). They should not be
        // treated as control events.
        let v = json!({
            "method": "vt_sequence",
            "params": { "pane_id": "p1", "tab_id": "T1" }
        });
        assert!(try_parse_internal(&v).unwrap().is_none());
    }

    #[test]
    fn returns_none_when_method_field_is_absent() {
        // Defensive: a payload that has no `method` at all is not our
        // problem to route. The caller's existing dispatcher will
        // surface the malformed event in its own way.
        let v = json!({ "params": {} });
        assert!(try_parse_internal(&v).unwrap().is_none());
    }

    // ───────── Error surfacing ─────────

    #[test]
    fn errors_on_malformed_internal_payload() {
        // Missing required `tab_id`. Surface this as Err so the
        // caller can log + ignore (rather than silently swallowing
        // a malformed control command).
        let v = json!({
            "method": "_internal.attach_pane",
            "params": {
                "pty_in": 1,
                "pty_out": 2,
                "agent_id": "claude",
                "initial_cwd": "/tmp",
                "initial_view": "chat",
            }
        });
        assert!(try_parse_internal(&v).is_err());
    }

    #[test]
    fn errors_on_unknown_internal_method() {
        // An _internal.* method we don't recognise is treated as
        // malformed: serde rejects unknown variants on tagged enums.
        // This guards against typos and protocol-drift between WT
        // and wta builds.
        let v = json!({
            "method": "_internal.unknown_thing",
            "params": {}
        });
        assert!(try_parse_internal(&v).is_err());
    }

    // ───────── Outbound serialization ─────────

    #[test]
    fn serializes_attach_pane_ack_with_correlation_id() {
        let ack = OutboundAck::AttachPaneAck {
            id: Some("abc-123".into()),
            params: AckParams {
                tab_id: "T7".into(),
                status: AckStatus::Ok,
                error: None,
            },
        };
        let v = serde_json::to_value(&ack).unwrap();
        assert_eq!(
            v,
            json!({
                "method": "_internal.attach_pane_ack",
                "id": "abc-123",
                "params": {
                    "tab_id": "T7",
                    "status": "ok",
                }
            })
        );
    }

    #[test]
    fn serializes_error_ack_includes_message() {
        let ack = OutboundAck::DetachPaneAck {
            id: None,
            params: AckParams {
                tab_id: "T7".into(),
                status: AckStatus::Error,
                error: Some("tab not attached".into()),
            },
        };
        let v = serde_json::to_value(&ack).unwrap();
        assert_eq!(
            v,
            json!({
                "method": "_internal.detach_pane_ack",
                "params": {
                    "tab_id": "T7",
                    "status": "error",
                    "error": "tab not attached",
                }
            })
        );
    }

    #[test]
    fn ack_roundtrips_through_serde() {
        // Wire format stability check: serializing an ack and
        // deserializing it back yields the same value. This catches
        // accidental field-rename drift.
        let original = OutboundAck::AttachPaneAck {
            id: Some("x".into()),
            params: AckParams {
                tab_id: "T1".into(),
                status: AckStatus::Ok,
                error: None,
            },
        };
        let s = serde_json::to_string(&original).unwrap();
        let back: OutboundAck = serde_json::from_str(&s).unwrap();
        assert_eq!(back, original);
    }
}
