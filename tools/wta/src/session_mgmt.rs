//! Pure-function core of session management Enter routing for the agent
//! session manager.
//!
//! `decide_enter_action` is a closed, side-effect-free mapping from a
//! `RowSnapshot` (the relevant projection of an `AgentSession` row plus
//! ambient capabilities) to an `EnterAction` — a
//! description of *what* the caller should dispatch (Focus, resume in a
//! new agent pane via ACP `session/load`, resume in a plain pane via the
//! CLI's own resume flag/verb, or surface a "not resumable" message).
//!
//! The actual dispatch (split-pane, wtcli, ACP load) lives in `app.rs`
//! and keeps the existing guard rails (phantom-session pruning, self-
//! focus skip, optimistic `ResumeDispatched` bumps, agent-pane
//! reconciliation). This module is intentionally tiny so the routing
//! table can be exhaustively unit-tested without spinning up any
//! runtime, futures, or mocks.
//!
//! State machine:
//!
//! ```text
//! Live + has pane_session_id    -> Focus { pane_session_id }
//! Live + no pane_session_id     -> NotResumable(LiveWithoutPane)
//!
//! Class A (origin=AgentPane), dead (Ended | Historical):
//!   Enter -> ResumeInAgentPane (needs load_session_supported)
//!
//! Class B (origin=Unknown), dead (Ended | Historical):
//!   Enter -> ResumeCliFlag      (needs cli_supports_resume_flag)
//!
//! WSL rows, dead -> ResumeCliFlag regardless of origin (the agent
//!                   pane is host-side, so ACP `session/load` can't
//!                   rehydrate an in-distro session)
//!
//! Cli Unknown in any dead branch -> NotResumable(UnknownCli)
//! Missing capability in the chosen branch -> NotResumable(<reason>)
//! ```
//!
//! Enter is the only activation gesture: a row has exactly one resume
//! style, decided by where the session came from. There is no second
//! route to fall back to, so a row whose single path is unavailable
//! reports `NotResumable` instead. The key handler only activates on a
//! bare Enter, so no modifier combination reaches this module.

use crate::agent_sessions::{AgentKey, CliSource, SessionOrigin};

/// Two-dimensional liveness of a session row. Carved off the legacy
/// one-dimensional `AgentStatus` so callers can express "session is
/// alive somewhere" independently of activity (Idle/Working/...).
///
/// This enum is the input we *want* the session management view to feed in; for now,
/// the caller can collapse `AgentStatus` into it (see
/// [`liveness_from_status`]). Once `agent_sessions::AgentSession` itself
/// splits to activity + liveness fields, this becomes a direct copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Liveness {
    /// The session is currently connected. `pane_session_id` is the
    /// WT pane GUID hosting it (Class A: from the master-pushed
    /// registry mirror; Class B: from hooks). `None` is only expected
    /// in a brief window between "we know the session is live" and
    /// "we've bound a pane GUID to it" — the caller treats that as
    /// `NotResumable { LiveWithoutPane }` so no half-baked focus call
    /// is issued.
    Live { pane_session_id: Option<String> },
    /// Session was Live in this WTA process at some point, then its
    /// pane/connection closed (`SessionStopped` / `PaneClosed`).
    Ended,
    /// Session was reconstructed from on-disk history; never connected
    /// during this WTA's lifetime.
    Historical,
}

/// Why a row can't be activated. Mapped to a user-visible toast/system
/// message by the caller (the message wording lives near the dispatch
/// code where it can reference `agent_name` and similar context).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotResumableReason {
    /// Live, but the row hasn't acquired a `pane_session_id` yet. Almost
    /// always a transient race; the user can retry Enter.
    LiveWithoutPane,
    /// Wanted `ResumeInAgentPane` but the connected agent didn't
    /// advertise the `loadSession` capability.
    LoadSessionNotSupported,
    /// Wanted `ResumeCliFlag` but the CLI has no `--resume`-style flag.
    CliHasNoResumeFlag,
    /// `CliSource::Unknown(_)` — we don't know how to spawn the CLI, so
    /// neither dead-row path applies.
    UnknownCli,
}

/// What the caller should actually do in response to Enter.
///
/// Pure data; no `Box<dyn FnOnce>` or futures. Dispatch (and its
/// existing guard rails) lives in `app.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnterAction {
    /// Hand off to `wtcli focus-pane <pane_session_id>`. With the
    /// stash-aware FocusProtocolPane change (PR A) landed, this
    /// transparently restores a stashed agent pane in addition to
    /// switching tab + focusing the TermControl.
    Focus { pane_session_id: String },
    /// Open a new WT tab + agent pane reconciled to `cli`, then issue
    /// ACP `session/load(key)` so the agent rehydrates the conversation
    /// in-place. Requires `load_session_supported`.
    ResumeInAgentPane { key: AgentKey, cli: CliSource },
    /// Open a new WT tab + plain pane running `<cli> <resume_flag>
    /// <key>` so the CLI itself rehydrates from its on-disk session
    /// store. Requires `cli_supports_resume_flag`.
    ResumeCliFlag { key: AgentKey, cli: CliSource },
    /// No path applies; caller surfaces a message and stays on the
    /// session manager view.
    NotResumable { reason: NotResumableReason },
}

/// The minimum information `decide_enter_action` needs. Constructed from
/// an `AgentSession` row plus ambient capabilities (which today live on
/// `App`). Kept small so unit tests stay obvious.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowSnapshot {
    pub origin: SessionOrigin,
    pub liveness: Liveness,
    pub key: AgentKey,
    pub cli_source: CliSource,
    /// Whether the connected agent (the one the helper is talking to via
    /// ACP) advertised the `loadSession` capability at initialize.
    pub load_session_supported: bool,
    /// Whether the CLI exposes a resume flag or verb of its own. The
    /// spelling varies per agent — `--resume` for Claude/Copilot/Gemini,
    /// the `resume` subcommand for Codex, `--session` for OpenCode — so
    /// this is sourced from `AgentProfile::resume_flag` rather than
    /// assumed.
    pub cli_supports_resume_flag: bool,
    /// Whether the session lives inside a WSL distro. The helper and
    /// the agent run on the host, so ACP `session/load` can't rehydrate
    /// a Linux session into a host agent pane — WSL rows always resume
    /// through the in-distro CLI's own resume flag.
    pub is_wsl: bool,
}

/// The state machine. See the module docstring for the table this
/// implements.
pub fn decide_enter_action(row: &RowSnapshot) -> EnterAction {
    match &row.liveness {
        Liveness::Live { pane_session_id } => match pane_session_id {
            Some(p) if !p.is_empty() => EnterAction::Focus {
                pane_session_id: p.clone(),
            },
            _ => EnterAction::NotResumable {
                reason: NotResumableReason::LiveWithoutPane,
            },
        },
        Liveness::Ended | Liveness::Historical => {
            // Unknown CLI: we don't know how to spawn it for either
            // path, regardless of origin.
            if matches!(row.cli_source, CliSource::Unknown(_)) {
                return EnterAction::NotResumable {
                    reason: NotResumableReason::UnknownCli,
                };
            }

            // One resume style per origin:
            //   Class A (AgentPane): ResumeInAgentPane.
            //   Class B (Unknown):   ResumeCliFlag.
            // WSL rows are always CLI-flag: the agent pane is host-side.
            let want_agent_pane = match row.origin {
                SessionOrigin::AgentPane => !row.is_wsl,
                SessionOrigin::Unknown => false,
            };

            if want_agent_pane {
                if row.load_session_supported {
                    EnterAction::ResumeInAgentPane {
                        key: row.key.clone(),
                        cli: row.cli_source.clone(),
                    }
                } else {
                    EnterAction::NotResumable {
                        reason: NotResumableReason::LoadSessionNotSupported,
                    }
                }
            } else if row.cli_supports_resume_flag {
                EnterAction::ResumeCliFlag {
                    key: row.key.clone(),
                    cli: row.cli_source.clone(),
                }
            } else {
                EnterAction::NotResumable {
                    reason: NotResumableReason::CliHasNoResumeFlag,
                }
            }
        }
    }
}

/// Bridge helper for the transition period before
/// `agent_sessions::AgentSession` itself splits to activity + liveness
/// fields. Collapses the legacy one-dimensional `AgentStatus` into a
/// `Liveness`, carrying the `pane_session_id` through for `Live` rows.
///
/// Mapping mirrors the `activate_agent_session_routed` dispatch
/// (`Idle | Working | Attention | Error` are all "live"; `Ended` and
/// `Historical` are the dead buckets).
pub fn liveness_from_status(
    status: &crate::agent_sessions::AgentStatus,
    pane_session_id: Option<String>,
) -> Liveness {
    use crate::agent_sessions::AgentStatus::*;
    match status {
        Idle | Working | Attention | Error => Liveness::Live { pane_session_id },
        Ended => Liveness::Ended,
        Historical => Liveness::Historical,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        origin: SessionOrigin,
        liveness: Liveness,
        cli: CliSource,
        load_session_supported: bool,
        cli_supports_resume_flag: bool,
    ) -> RowSnapshot {
        RowSnapshot {
            origin,
            liveness,
            key: "k".to_string(),
            cli_source: cli,
            load_session_supported,
            cli_supports_resume_flag,
            is_wsl: false,
        }
    }

    // --- Live rows ---------------------------------------------------

    #[test]
    fn class_a_live_with_pane_enter_focuses() {
        let r = row(
            SessionOrigin::AgentPane,
            Liveness::Live {
                pane_session_id: Some("pane-A".into()),
            },
            CliSource::Copilot,
            true,
            true,
        );
        assert_eq!(
            decide_enter_action(&r),
            EnterAction::Focus {
                pane_session_id: "pane-A".into()
            }
        );
    }

    #[test]
    fn codex_class_a_live_with_pane_enter_focuses() {
        let r = row(
            SessionOrigin::AgentPane,
            Liveness::Live {
                pane_session_id: Some("pane-A".into()),
            },
            CliSource::Codex,
            true,
            true,
        );
        assert_eq!(
            decide_enter_action(&r),
            EnterAction::Focus {
                pane_session_id: "pane-A".into()
            }
        );
    }

    #[test]
    fn class_b_live_with_pane_enter_focuses() {
        let r = row(
            SessionOrigin::Unknown,
            Liveness::Live {
                pane_session_id: Some("pane-B".into()),
            },
            CliSource::Claude,
            false,
            true,
        );
        assert_eq!(
            decide_enter_action(&r),
            EnterAction::Focus {
                pane_session_id: "pane-B".into()
            }
        );
    }

    #[test]
    fn live_without_pane_not_resumable() {
        let r = row(
            SessionOrigin::AgentPane,
            Liveness::Live {
                pane_session_id: None,
            },
            CliSource::Copilot,
            true,
            true,
        );
        assert_eq!(
            decide_enter_action(&r),
            EnterAction::NotResumable {
                reason: NotResumableReason::LiveWithoutPane
            }
        );
    }

    #[test]
    fn live_with_empty_pane_not_resumable() {
        // Defensive: callers should always provide either Some(non-empty)
        // or None, but Some("") shouldn't slip through as a focus call.
        let r = row(
            SessionOrigin::Unknown,
            Liveness::Live {
                pane_session_id: Some(String::new()),
            },
            CliSource::Claude,
            false,
            true,
        );
        assert_eq!(
            decide_enter_action(&r),
            EnterAction::NotResumable {
                reason: NotResumableReason::LiveWithoutPane
            }
        );
    }

    // --- Dead rows: Class A ------------------------------------------

    #[test]
    fn class_a_ended_enter_resumes_in_agent_pane_when_supported() {
        let r = row(
            SessionOrigin::AgentPane,
            Liveness::Ended,
            CliSource::Copilot,
            true,
            true,
        );
        assert_eq!(
            decide_enter_action(&r),
            EnterAction::ResumeInAgentPane {
                key: "k".into(),
                cli: CliSource::Copilot
            }
        );
    }

    #[test]
    fn codex_class_a_ended_enter_resumes_in_agent_pane_when_supported() {
        let r = row(
            SessionOrigin::AgentPane,
            Liveness::Ended,
            CliSource::Codex,
            true,
            true,
        );
        assert_eq!(
            decide_enter_action(&r),
            EnterAction::ResumeInAgentPane {
                key: "k".into(),
                cli: CliSource::Codex
            }
        );
    }

    #[test]
    fn class_a_ended_enter_not_resumable_when_load_unsupported() {
        let r = row(
            SessionOrigin::AgentPane,
            Liveness::Ended,
            CliSource::Copilot,
            false, // load_session not supported
            true,
        );
        assert_eq!(
            decide_enter_action(&r),
            EnterAction::NotResumable {
                reason: NotResumableReason::LoadSessionNotSupported
            }
        );
    }

    #[test]
    fn codex_class_a_ended_enter_not_resumable_when_load_unsupported() {
        let r = row(
            SessionOrigin::AgentPane,
            Liveness::Ended,
            CliSource::Codex,
            false, // load_session not supported
            true,
        );
        assert_eq!(
            decide_enter_action(&r),
            EnterAction::NotResumable {
                reason: NotResumableReason::LoadSessionNotSupported
            }
        );
    }

    #[test]
    fn class_a_historical_enter_routes_like_ended() {
        let r = row(
            SessionOrigin::AgentPane,
            Liveness::Historical,
            CliSource::Gemini,
            true,
            true,
        );
        assert_eq!(
            decide_enter_action(&r),
            EnterAction::ResumeInAgentPane {
                key: "k".into(),
                cli: CliSource::Gemini
            }
        );
    }

    #[test]
    fn codex_class_a_historical_enter_routes_like_ended() {
        let r = row(
            SessionOrigin::AgentPane,
            Liveness::Historical,
            CliSource::Codex,
            true,
            true,
        );
        assert_eq!(
            decide_enter_action(&r),
            EnterAction::ResumeInAgentPane {
                key: "k".into(),
                cli: CliSource::Codex
            }
        );
    }

    // --- Dead rows: Class B ------------------------------------------

    #[test]
    fn class_b_historical_enter_resumes_via_cli_flag() {
        let r = row(
            SessionOrigin::Unknown,
            Liveness::Historical,
            CliSource::Copilot,
            true,
            true,
        );
        assert_eq!(
            decide_enter_action(&r),
            EnterAction::ResumeCliFlag {
                key: "k".into(),
                cli: CliSource::Copilot
            }
        );
    }

    #[test]
    fn class_b_historical_enter_not_resumable_when_cli_has_no_flag() {
        let r = row(
            SessionOrigin::Unknown,
            Liveness::Historical,
            CliSource::Copilot,
            true,
            false,
        );
        assert_eq!(
            decide_enter_action(&r),
            EnterAction::NotResumable {
                reason: NotResumableReason::CliHasNoResumeFlag
            }
        );
    }

    #[test]
    fn class_b_ended_routes_like_class_b_historical() {
        // Ended can occur for Class B too — a hook-discovered live
        // session whose pane just closed. Same routing as Historical.
        let r = row(
            SessionOrigin::Unknown,
            Liveness::Ended,
            CliSource::Copilot,
            true,
            true,
        );
        assert_eq!(
            decide_enter_action(&r),
            EnterAction::ResumeCliFlag {
                key: "k".into(),
                cli: CliSource::Copilot
            }
        );
    }

    // --- Dead rows: WSL ----------------------------------------------

    #[test]
    fn wsl_dead_row_resumes_via_cli_flag_even_for_agent_pane_origin() {
        // The helper and the agent are host-side, so ACP `session/load`
        // can't rehydrate an in-distro session into an agent pane.
        let mut r = row(
            SessionOrigin::AgentPane,
            Liveness::Ended,
            CliSource::Claude,
            true,
            true,
        );
        r.is_wsl = true;
        assert_eq!(
            decide_enter_action(&r),
            EnterAction::ResumeCliFlag {
                key: "k".into(),
                cli: CliSource::Claude
            }
        );
    }

    // --- Dead rows: Unknown CLI --------------------------------------

    #[test]
    fn unknown_cli_not_resumable_for_any_origin_or_liveness() {
        for origin in [SessionOrigin::AgentPane, SessionOrigin::Unknown] {
            for liveness in [Liveness::Ended, Liveness::Historical] {
                let r = row(
                    origin.clone(),
                    liveness.clone(),
                    CliSource::Unknown("weird".into()),
                    true,
                    true,
                );
                assert_eq!(
                    decide_enter_action(&r),
                    EnterAction::NotResumable {
                        reason: NotResumableReason::UnknownCli
                    },
                    "origin={origin:?} liveness={liveness:?}",
                );
            }
        }
    }

    // --- Bridge helper -----------------------------------------------

    #[test]
    fn liveness_from_status_maps_activity_states_to_live() {
        use crate::agent_sessions::AgentStatus::*;
        for s in [Idle, Working, Attention, Error] {
            assert_eq!(
                liveness_from_status(&s, Some("pane".into())),
                Liveness::Live {
                    pane_session_id: Some("pane".into())
                },
                "status={s:?}"
            );
        }
    }

    #[test]
    fn liveness_from_status_maps_terminal_states() {
        use crate::agent_sessions::AgentStatus::*;
        assert_eq!(
            liveness_from_status(&Ended, Some("ignored".into())),
            Liveness::Ended
        );
        assert_eq!(
            liveness_from_status(&Historical, None),
            Liveness::Historical
        );
    }

    #[test]
    fn liveness_from_status_threads_pane_session_id_through_for_live() {
        use crate::agent_sessions::AgentStatus::*;
        assert_eq!(
            liveness_from_status(&Idle, None),
            Liveness::Live {
                pane_session_id: None
            }
        );
    }
}
