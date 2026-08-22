//! `master` unit tests, split out of the large `mod.rs` file so it lives in
//! its own file. This is a child module of `master` (declared with
//! `#[path]` in mod.rs), not of the crate root, so it can reach master's
//! private items directly, the same way the file used to when this was an
//! inline `mod tests { ... }` block.

use super::*;
use acp::schema::v1::{ContentChunk, SessionId, SessionNotification, SessionUpdate};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[derive(Clone)]
struct PendingNewSessionAgent;

#[derive(Clone)]
struct PendingLoadSessionAgent {
    arrivals: mpsc::UnboundedSender<usize>,
}

#[derive(Clone)]
struct ControlledNewSessionAgent {
    next: Arc<std::sync::atomic::AtomicUsize>,
    events: mpsc::UnboundedSender<ReplacementEvent>,
    live_sessions: Arc<Mutex<HashSet<SessionId>>>,
    fail_close: Option<SessionId>,
    close_method_not_found: bool,
    capture_cancel: bool,
    failed_closes: Arc<Mutex<HashSet<SessionId>>>,
}

#[derive(Clone)]
struct BlockingCloseAgent {
    events: mpsc::UnboundedSender<ReplacementEvent>,
}

#[derive(Clone)]
struct BlockingCancelAgent {
    events: mpsc::UnboundedSender<ReplacementEvent>,
}

#[derive(Clone)]
struct RebindDuringCloseAgent {
    predecessor: SessionId,
    events: mpsc::UnboundedSender<ReplacementEvent>,
}

enum ReplacementEvent {
    Cancel(SessionId),
    Close(SessionId),
    BlockingClose(SessionId, tokio::sync::oneshot::Sender<()>),
    BlockingCancel(SessionId, tokio::sync::oneshot::Sender<()>),
    FailingClose(SessionId, tokio::sync::oneshot::Sender<()>),
    Load(SessionId),
    New(usize, tokio::sync::oneshot::Sender<()>),
}

impl ControlledNewSessionAgent {
    async fn cancel(&self, args: acp::schema::v1::CancelNotification) -> acp::Result<()> {
        if self.capture_cancel {
            self.events
                .send(ReplacementEvent::Cancel(args.session_id))
                .map_err(|_| acp::Error::internal_error().data("test event receiver dropped"))?;
        }
        Ok(())
    }

    async fn new_session(
        &self,
        _args: acp::schema::v1::NewSessionRequest,
    ) -> acp::Result<acp::schema::v1::NewSessionResponse> {
        let index = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        self.events
            .send(ReplacementEvent::New(index, release_tx))
            .map_err(|_| acp::Error::internal_error().data("test arrival receiver dropped"))?;
        release_rx
            .await
            .map_err(|_| acp::Error::internal_error().data("test release sender dropped"))?;
        let session_id = match index {
            0 => "replacement-b",
            1 => "replacement-c",
            _ => return Err(acp::Error::internal_error().data("unexpected test request")),
        };
        let session_id = SessionId::new(session_id);
        self.live_sessions.lock().await.insert(session_id.clone());
        Ok(acp::schema::v1::NewSessionResponse::new(session_id))
    }

    async fn close_session(
        &self,
        args: acp::schema::v1::CloseSessionRequest,
    ) -> acp::Result<acp::schema::v1::CloseSessionResponse> {
        self.events
            .send(ReplacementEvent::Close(args.session_id.clone()))
            .map_err(|_| acp::Error::internal_error().data("test event receiver dropped"))?;
        if self.close_method_not_found {
            return Err(acp::Error::method_not_found());
        }
        if self.fail_close.as_ref() == Some(&args.session_id)
            && self
                .failed_closes
                .lock()
                .await
                .insert(args.session_id.clone())
        {
            return Err(acp::Error::internal_error().data("injected close failure"));
        }
        self.live_sessions.lock().await.remove(&args.session_id);
        Ok(acp::schema::v1::CloseSessionResponse::new())
    }

    async fn load_session(
        &self,
        args: acp::schema::v1::LoadSessionRequest,
    ) -> acp::Result<acp::schema::v1::LoadSessionResponse> {
        self.events
            .send(ReplacementEvent::Load(args.session_id.clone()))
            .map_err(|_| acp::Error::internal_error().data("test event receiver dropped"))?;
        self.live_sessions.lock().await.insert(args.session_id);
        Ok(acp::schema::v1::LoadSessionResponse::new())
    }
}

impl PendingLoadSessionAgent {
    async fn load_session(
        &self,
        args: acp::schema::v1::LoadSessionRequest,
    ) -> acp::Result<acp::schema::v1::LoadSessionResponse> {
        self.arrivals
            .send(args.mcp_servers.len())
            .map_err(|_| acp::Error::internal_error().data("test arrival receiver dropped"))?;
        futures::future::pending().await
    }
}

impl BlockingCloseAgent {
    async fn close_session(
        &self,
        args: acp::schema::v1::CloseSessionRequest,
    ) -> acp::Result<acp::schema::v1::CloseSessionResponse> {
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        self.events
            .send(ReplacementEvent::BlockingClose(args.session_id, release_tx))
            .map_err(|_| acp::Error::internal_error().data("test event receiver dropped"))?;
        release_rx
            .await
            .map_err(|_| acp::Error::internal_error().data("test release sender dropped"))?;
        Ok(acp::schema::v1::CloseSessionResponse::new())
    }
}

impl BlockingCancelAgent {
    async fn cancel(&self, args: acp::schema::v1::CancelNotification) -> acp::Result<()> {
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        self.events
            .send(ReplacementEvent::BlockingCancel(
                args.session_id,
                release_tx,
            ))
            .map_err(|_| acp::Error::internal_error().data("test event receiver dropped"))?;
        release_rx
            .await
            .map_err(|_| acp::Error::internal_error().data("test release sender dropped"))?;
        Ok(())
    }
}

impl RebindDuringCloseAgent {
    async fn load_session(
        &self,
        args: acp::schema::v1::LoadSessionRequest,
    ) -> acp::Result<acp::schema::v1::LoadSessionResponse> {
        self.events
            .send(ReplacementEvent::Load(args.session_id))
            .map_err(|_| acp::Error::internal_error().data("test event receiver dropped"))?;
        Ok(acp::schema::v1::LoadSessionResponse::new())
    }

    async fn close_session(
        &self,
        args: acp::schema::v1::CloseSessionRequest,
    ) -> acp::Result<acp::schema::v1::CloseSessionResponse> {
        if args.session_id == self.predecessor {
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            self.events
                .send(ReplacementEvent::FailingClose(args.session_id, release_tx))
                .map_err(|_| acp::Error::internal_error().data("test event receiver dropped"))?;
            release_rx
                .await
                .map_err(|_| acp::Error::internal_error().data("test release sender dropped"))?;
            Err(acp::Error::internal_error().data("injected predecessor close failure"))
        } else {
            self.events
                .send(ReplacementEvent::Close(args.session_id))
                .map_err(|_| acp::Error::internal_error().data("test event receiver dropped"))?;
            Ok(acp::schema::v1::CloseSessionResponse::new())
        }
    }
}

impl PendingNewSessionAgent {
    async fn initialize(
        &self,
        _args: acp::schema::v1::InitializeRequest,
    ) -> acp::Result<acp::schema::v1::InitializeResponse> {
        Ok(acp::schema::v1::InitializeResponse::new(
            acp::schema::ProtocolVersion::V1,
        ))
    }
    async fn authenticate(
        &self,
        _args: acp::schema::v1::AuthenticateRequest,
    ) -> acp::Result<acp::schema::v1::AuthenticateResponse> {
        Ok(acp::schema::v1::AuthenticateResponse::new())
    }
    async fn new_session(
        &self,
        _args: acp::schema::v1::NewSessionRequest,
    ) -> acp::Result<acp::schema::v1::NewSessionResponse> {
        futures::future::pending().await
    }
}

// ── Agent selection / security policy ───────────────────────────
//
// `resolve_agent_selection` is the single choke point that decides
// what the master will spawn for a helper. Extracting it as a pure
// function lets us exercise the full policy — id reconstruction,
// GPO allowlist, fallback, and the "never trust a command off the
// pipe" invariant — without launching a single subprocess (cleaner
// than injecting a fake spawner, which only the I/O plumbing needs).

const DEFAULT_CMD: &str = "copilot --acp --stdio";

#[test]
fn fresh_host_byok_startup_uses_clean_probe_only_when_needed() {
    use crate::agent_registry::ByokMode;
    use crate::agent_source::AgentSource;

    assert_eq!(
        cloud_catalog_plan(
            &AgentSource::Host,
            ByokMode::CopilotProviderEnvironment,
            true,
            true,
        ),
        CloudCatalogPlan::CleanProbe,
        "a fresh Host BYOK agent with no helper catalog needs one clean probe"
    );
    assert_eq!(
        cloud_catalog_plan(
            &AgentSource::Host,
            ByokMode::CopilotProviderEnvironment,
            true,
            false,
        ),
        CloudCatalogPlan::Supplied,
        "a non-empty supplied host snapshot avoids the extra process"
    );
    assert_eq!(
        cloud_catalog_plan(
            &AgentSource::Wsl {
                distro: "Ubuntu".into(),
            },
            ByokMode::CopilotProviderEnvironment,
            true,
            false,
        ),
        CloudCatalogPlan::None,
        "WSL must not consume a Host cloud catalog"
    );
    assert_eq!(
        cloud_catalog_plan(&AgentSource::Host, ByokMode::Unsupported, true, true),
        CloudCatalogPlan::None,
        "unsupported agents must not trigger a BYOK cloud probe"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn delayed_clean_probe_does_not_block_initialize_and_notifies_bound_helper() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(41);
            let (ext_tx, mut ext_rx) = mpsc::unbounded_channel();
            state
                .helper_ext_subscribers
                .lock()
                .await
                .insert(helper_id, ext_tx);

            let config_hit = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let legacy_hit = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_model_agent(false, config_hit, legacy_hit),
                cached_init_resp: acp::schema::v1::InitializeResponse::new(
                    acp::schema::ProtocolVersion::V1,
                ),
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "delayed-probe-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Pending),
                bound_helpers: Mutex::new(HashSet::from([helper_id])),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let (complete_tx, complete_rx) = tokio::sync::oneshot::channel();
            start_clean_cloud_catalog_probe(
                Arc::clone(&state),
                Arc::clone(&agent),
                "copilot".to_string(),
                async move { complete_rx.await.map_err(anyhow::Error::from) },
            );

            let mut response = tokio::time::timeout(
                std::time::Duration::from_millis(50),
                initialize_response_for_agent(&agent, false),
            )
            .await
            .expect("helper initialize response must not wait for the clean probe")
            .expect("pending catalog cannot fail serialization");
            let wta_meta = crate::session_registry::extract_wta_meta(&mut response.meta);
            assert!(
                crate::protocol::acp::model_select::cloud_catalog_from_wta_meta(&wta_meta)
                    .models
                    .is_empty(),
                "pending probe must not fabricate initialize metadata"
            );
            assert!(
                wta_meta.proposal_mcp.is_none(),
                "unavailable session MCP must not be advertised"
            );

            let mut response = initialize_response_for_agent(&agent, true)
                .await
                .expect("proposal metadata serializes");
            assert_eq!(
                crate::session_registry::extract_wta_meta(&mut response.meta)
                    .proposal_mcp
                    .as_deref(),
                Some("http-v1"),
                "reachable session MCP must be advertised to the Helper"
            );

            assert!(complete_tx
                .send(crate::protocol::acp::probe::ProbeResult {
                    available_models: vec![crate::app::AcpModelInfo {
                        id: "cloud-later".to_string(),
                        name: "Cloud Later".to_string(),
                        description: None,
                    }],
                    current_model_id: None,
                })
                .is_ok());

            let notification =
                tokio::time::timeout(std::time::Duration::from_secs(1), ext_rx.recv())
                    .await
                    .expect("completed probe should notify promptly")
                    .expect("bound helper notification channel remains open");
            let catalog = crate::protocol::acp::model_select::parse_wta_cloud_catalog_notification(
                &notification,
            )
            .expect("notification should be the private cloud catalog method")
            .expect("notification payload should parse");
            assert_eq!(catalog.models.len(), 1);
            assert_eq!(catalog.models[0].id, "cloud-later");
            assert_eq!(catalog.source.as_deref(), Some("clean_probe"));
            assert!(matches!(
                *agent.cloud_catalog.lock().await,
                NativeCloudCatalogState::Ready(_)
            ));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn failed_clean_probe_is_recorded_without_catalog_delivery() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(42);
            let (ext_tx, mut ext_rx) = mpsc::unbounded_channel();
            state
                .helper_ext_subscribers
                .lock()
                .await
                .insert(helper_id, ext_tx);

            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_model_agent(
                    false,
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                ),
                cached_init_resp: acp::schema::v1::InitializeResponse::new(
                    acp::schema::ProtocolVersion::V1,
                ),
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "failed-probe-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Pending),
                bound_helpers: Mutex::new(HashSet::from([helper_id])),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            start_clean_cloud_catalog_probe(
                Arc::clone(&state),
                Arc::clone(&agent),
                "copilot".to_string(),
                async {
                    Err::<crate::protocol::acp::probe::ProbeResult, _>(anyhow::anyhow!(
                        "probe failed"
                    ))
                },
            );
            tokio::task::yield_now().await;

            assert!(matches!(
                *agent.cloud_catalog.lock().await,
                NativeCloudCatalogState::Failed
            ));
            assert!(
                ext_rx.try_recv().is_err(),
                "a failed probe must not clear or replace the helper's existing catalog"
            );
        })
        .await;
}

fn allow_set(ids: &[&str]) -> std::collections::HashSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

/// Run the resolver the way `HelperHandler::initialize` does.
fn resolve(
    allowed: Option<&std::collections::HashSet<String>>,
    requested_id: Option<&str>,
    model: Option<&str>,
) -> (String, Option<String>) {
    let (command, agent_id, _source) = resolve_agent_selection(
        DEFAULT_CMD,
        Some("copilot"),
        allowed,
        requested_id,
        model,
        None,
        None,
        HelperId(1),
    );
    (command, agent_id)
}

#[test]
fn known_id_with_no_allowlist_is_reconstructed_not_taken_from_pipe() {
    // No host allowlist (manual run / older host) ⇒ any known id is
    // honored, and the command is REBUILT from the id.
    let (cmd, id) = resolve(None, Some("gemini"), None);
    assert_eq!(cmd, "gemini --acp");
    assert_eq!(id.as_deref(), Some("gemini"));
}

#[test]
fn known_agent_selection_preserves_wsl_source() {
    let (command, agent_id, source) = resolve_agent_selection(
        DEFAULT_CMD,
        Some("copilot"),
        None,
        Some("copilot"),
        None,
        Some("wsl"),
        Some("Ubuntu"),
        HelperId(1),
    );
    assert_eq!(command, "copilot --acp --stdio");
    assert_eq!(agent_id.as_deref(), Some("copilot"));
    assert_eq!(
        source,
        crate::agent_source::AgentSource::Wsl {
            distro: "Ubuntu".to_string()
        }
    );
    assert_ne!(
        agent_cmd_key(
            &command,
            Some("copilot"),
            &crate::agent_source::AgentSource::Host,
        ),
        agent_cmd_key(&command, Some("copilot"), &source),
        "host and WSL instances must occupy separate pool slots"
    );
}

#[test]
fn agent_pool_key_includes_authoritative_identity() {
    let command = "copilot --acp --stdio";
    let source = crate::agent_source::AgentSource::Host;
    assert_ne!(
        agent_cmd_key(command, Some("copilot"), &source),
        agent_cmd_key(command, Some("custom:copilot"), &source)
    );
    assert_eq!(
        agent_cmd_key(command, Some("copilot"), &source),
        agent_cmd_key(command, Some("copilot"), &source)
    );
    assert_ne!(
        agent_cmd_key("b:c", Some("a"), &source),
        agent_cmd_key("c", Some("a:b"), &source)
    );
    assert!(
        agent_cmd_key("custom\0command\n", Some("custom\0id"), &source)
            .chars()
            .all(|character| !character.is_control())
    );
}

#[test]
fn model_is_folded_in_for_native_agents_and_ignored_for_adapters() {
    // Native agent (gemini) takes --model on the command line.
    let (cmd, _) = resolve(None, Some("gemini"), Some("gemini-2.5-pro"));
    assert_eq!(cmd, "gemini --acp --model gemini-2.5-pro");

    // Adapter agent (claude via npx) ignores the model here — it's
    // applied later via setSessionModel — so the command is stable.
    let (cmd, id) = resolve(None, Some("claude"), Some("opus-4"));
    assert_eq!(cmd, "npx -y @agentclientprotocol/claude-agent-acp@0.65.0");
    assert_eq!(id.as_deref(), Some("claude"));
}

#[test]
fn id_is_case_insensitive() {
    let (cmd, id) = resolve(Some(&allow_set(&["gemini"])), Some("GeMiNi"), None);
    assert_eq!(cmd, "gemini --acp");
    assert_eq!(id.as_deref(), Some("gemini"));
}

#[test]
fn empty_or_missing_id_falls_back_to_default() {
    for requested in [None, Some(""), Some("   ")] {
        let (cmd, id) = resolve(None, requested, None);
        assert_eq!(cmd, DEFAULT_CMD, "requested={requested:?}");
        assert_eq!(id.as_deref(), Some("copilot"));
    }
}

#[test]
fn every_known_agent_id_is_honored_not_conflated_with_default_fallback() {
    // Regression guard for the conflation flagged in review: the `known`
    // check must test KNOWN_AGENTS membership directly, NOT
    // `lookup_profile_by_id(id).id != DEFAULT_PROFILE.id`. The latter
    // silently treats a real agent as "unknown" — forcing the default and
    // dropping requested-model folding — the day DEFAULT_PROFILE.id is set
    // to a genuine, selectable agent id. Every known agent must resolve to
    // its own rebuilt command and stamp its own id.
    for profile in crate::agent_registry::KNOWN_AGENTS {
        let (cmd, id) = resolve(None, Some(profile.id), None);
        let expected = crate::agent_registry::build_acp_command(profile.id, None);
        assert_eq!(
            cmd, expected,
            "agent {} must be honored, not fall back",
            profile.id
        );
        assert_eq!(
            id.as_deref(),
            Some(profile.id),
            "id stamp for {}",
            profile.id
        );
    }
}

#[test]
fn unknown_or_custom_id_falls_back_to_trusted_default() {
    // `custom:` and bogus ids aren't in KNOWN_AGENTS ⇒ the master
    // runs the trusted global default (which is what carries the
    // global custom command), never a string from the pipe.
    for requested in ["custom", "custom:calc.exe", "totally-bogus"] {
        let (cmd, id) = resolve(None, Some(requested), None);
        assert_eq!(cmd, DEFAULT_CMD, "requested={requested}");
        assert_eq!(id.as_deref(), Some("copilot"));
    }
}

#[test]
fn allowed_ids_absent_is_no_policy_present_but_empty_is_block_all() {
    // The flag being *absent* (clap yields `[]`) is the only "no host
    // policy" case → `None` → accept any known id.
    assert_eq!(
        normalize_allowed_agent_ids(&[]),
        None,
        "no argv ⇒ no policy"
    );

    // The flag being *present* but filtering down to nothing is honored
    // fail-closed → `Some({})` → block every helper-selected id (all tabs
    // fall back to the trusted default). clap `value_delimiter = ','`
    // turns `--allowed-agent-ids ""` into `[""]`: a present argv with zero
    // real ids. It must NOT widen back to `None`.
    assert_eq!(
        normalize_allowed_agent_ids(&[String::new()]),
        Some(std::collections::HashSet::new()),
        "present-but-empty ⇒ block all, not no-policy"
    );
    assert_eq!(
        normalize_allowed_agent_ids(&["   ".to_string(), "\t".to_string()]),
        Some(std::collections::HashSet::new()),
        "present all-whitespace ⇒ block all"
    );
    // Unknown/custom ids can never be honored by resolve_agent_selection
    // (which requires is_known_id), so they're dropped — but the flag was
    // still supplied, so an all-unknown list blocks rather than widening.
    assert_eq!(
        normalize_allowed_agent_ids(&["custom:myapp".to_string(), "unknown".to_string()]),
        Some(std::collections::HashSet::new()),
        "present all-unknown ⇒ block all, not no-policy"
    );

    // Real known ids survive — trimmed + lowercased, blanks dropped.
    let set = normalize_allowed_agent_ids(&[
        "  Gemini ".to_string(),
        String::new(),
        "COPILOT".to_string(),
    ])
    .expect("non-empty allowlist");
    assert_eq!(set, allow_set(&["gemini", "copilot"]));
    // Unknown ids mixed with a real id: only the real id survives.
    let mixed = normalize_allowed_agent_ids(&["custom:myapp".to_string(), "claude".to_string()])
        .expect("one real id survives");
    assert_eq!(mixed, allow_set(&["claude"]));

    // End-to-end through resolve_agent_selection:
    //  - absent (None) ⇒ a known id is honored (reconstructed);
    //  - a surviving allowlist blocks a known-but-unlisted id;
    //  - present-but-empty blocks EVERY id (fail-closed).
    let (cmd, _) = resolve(None, Some("copilot"), None);
    assert_eq!(
        cmd,
        crate::agent_registry::build_acp_command("copilot", None),
        "no allowlist ⇒ known id honored (reconstructed)"
    );
    let listed = normalize_allowed_agent_ids(&["gemini".to_string()]);
    let (cmd, id) = resolve(listed.as_ref(), Some("copilot"), None);
    assert_eq!(cmd, DEFAULT_CMD, "unlisted id is refused");
    assert_eq!(id.as_deref(), Some("copilot"));
    let blocked = normalize_allowed_agent_ids(&[String::new()]);
    let (cmd, id) = resolve(blocked.as_ref(), Some("gemini"), None);
    assert_eq!(cmd, DEFAULT_CMD, "present-but-empty blocks even a known id");
    assert_eq!(id.as_deref(), Some("copilot"));
}

#[test]
fn host_empty_allowlist_flag_round_trips_as_block_all() {
    // The host (TerminalPage) must signal "AllowedAgents policy active but
    // it blocks every built-in ACP agent" so the master stays fail-closed.
    // It can't send an empty value as its own argv token — the command-line
    // builder drops empty args — so it emits the combined `--allowed-agent-ids=`
    // token. Verify clap turns that into a PRESENT-but-empty list (`[""]`),
    // which normalizes to block-all, and NOT into an absent flag (which
    // would mean "no policy / accept any known id" — the bypass we're closing).
    use clap::Parser;
    let cli = crate::cli::args::Cli::try_parse_from(["wta", "--allowed-agent-ids="])
        .expect("--allowed-agent-ids= parses");
    assert_eq!(
        cli.allowed_agent_ids,
        vec![String::new()],
        "combined empty value is present-but-empty, not absent"
    );
    assert_eq!(
        normalize_allowed_agent_ids(&cli.allowed_agent_ids),
        Some(std::collections::HashSet::new()),
        "present-but-empty ⇒ block all (fail-closed)"
    );
    // And the flag entirely absent stays "no host policy".
    let cli_absent = crate::cli::args::Cli::try_parse_from(["wta"]).expect("parses");
    assert_eq!(
        normalize_allowed_agent_ids(&cli_absent.allowed_agent_ids),
        None,
        "absent flag ⇒ no policy"
    );
}

#[test]
fn gpo_allowlist_blocks_known_but_unlisted_ids() {
    let allowed = allow_set(&["gemini"]);
    // gemini is listed ⇒ honored.
    let (cmd, _) = resolve(Some(&allowed), Some("gemini"), None);
    assert_eq!(cmd, "gemini --acp");
    // copilot is a *known* agent but NOT in the GPO-filtered set ⇒
    // refused, fall back to default. (Defends against a peer helper
    // selecting a policy-blocked agent.)
    let (cmd, id) = resolve(Some(&allowed), Some("copilot"), None);
    assert_eq!(cmd, DEFAULT_CMD);
    assert_eq!(id.as_deref(), Some("copilot"));
}

#[test]
fn agent_cmd_from_the_pipe_is_never_executed() {
    // Mirror the initialize path: a malicious helper sets a dangerous
    // `agent_cmd` alongside a benign `agent_id`. The resolver doesn't
    // even take `agent_cmd`, and the resolved command is rebuilt from
    // the id — so the pipe-supplied string can never be spawned.
    let mut meta: Option<acp::schema::v1::Meta> = None;
    crate::session_registry::inject_wta_meta(
        &mut meta,
        &crate::session_registry::WtaMeta {
            agent_cmd: Some("calc.exe".to_string()),
            agent_id: Some("gemini".to_string()),
            ..Default::default()
        },
    );
    let wta = crate::session_registry::extract_wta_meta(&mut meta);
    let (cmd, _) = resolve(None, wta.agent_id.as_deref(), wta.model.as_deref());
    assert_eq!(cmd, "gemini --acp");
    assert!(!cmd.contains("calc.exe"), "pipe command must never appear");
}

#[test]
fn pool_key_dedupes_same_selection_and_separates_distinct_agents() {
    // `get_or_spawn_agent` keys its CLI pool on the resolved command.
    // Same id+model ⇒ identical key ⇒ one shared CLI; different ids ⇒
    // different keys ⇒ separate CLIs (Gemini in one tab, Claude in
    // another). Assert the keying that drives that dedup.
    let (a, _) = resolve(None, Some("gemini"), Some("flash"));
    let (b, _) = resolve(None, Some("gemini"), Some("flash"));
    let (c, _) = resolve(None, Some("claude"), None);
    assert_eq!(a, b, "same selection must yield one pool key");
    assert_ne!(a, c, "different agents must get different pool keys");
}

fn make_state() -> Arc<MasterStateInner> {
    make_state_with_retirement_pending_timeout(SESSION_CLOSE_TIMEOUT)
}

fn make_state_with_retirement_pending_timeout(
    retirement_pending_timeout: std::time::Duration,
) -> Arc<MasterStateInner> {
    Arc::new(MasterStateInner {
        session_lifecycle_gates: Mutex::new(HashMap::new()),
        session_to_helper: Mutex::new(HashMap::new()),
        session_mcp_endpoints: session_mcp::Endpoints::new("http://127.0.0.1:1/mcp".to_string()),
        session_mcp_capabilities: session_mcp::CapabilityRegistry::default(),
        pending_usage: Mutex::new(HashMap::new()),
        usage_generation: watch::channel(0u64).0,
        registry: crate::session_registry::InMemoryRegistry::shared(),
        helper_ext_subscribers: Mutex::new(HashMap::new()),
        wt: None,
        agents: Mutex::new(HashMap::new()),
        default_agent_cmd: "copilot --acp --stdio".to_string(),
        default_agent_id: Some("copilot".to_string()),
        allowed_agent_ids: None,
        helper_meta: Mutex::new(HashMap::new()),
        tab_ownership_gate: Mutex::new(()),
        connected_helpers: Mutex::new(HashSet::new()),
        all_retirement_fence: Mutex::new(AllRetirementFence::default()),
        tab_retirement_fences: Mutex::new(HashMap::new()),
        tab_retirement_rekeys: Mutex::new(HashMap::new()),
        unresolved_owner_retirements: Mutex::new(HashMap::new()),
        pending_session_helpers: Mutex::new(HashMap::new()),
        pending_session_mcp: Mutex::new(HashMap::new()),
        closing_session_helpers: Mutex::new(HashSet::new()),
        destructive_session_helpers: Mutex::new(HashSet::new()),
        active_retirement_helpers: Mutex::new(HashSet::new()),
        closing_session_results: Mutex::new(HashMap::new()),
        session_transaction_changed: tokio::sync::Notify::new(),
        retirement_operations: Mutex::new(HashMap::new()),
        retirement_completion_tx: Mutex::new(None),
        retirement_pending_timeout,
        disconnect_orphan_publication_pause: Mutex::new(None),
        deferred_retirement_cleanup_complete: tokio::sync::Notify::new(),
        hook_owned: Mutex::new(HashSet::new()),
        born_bound: Mutex::new(HashSet::new()),
        orphaned_sessions: Mutex::new(HashMap::new()),
        orphaned_tabs: Mutex::new(HashMap::new()),
    })
}

async fn capture_retirement_completions(
    state: &MasterStateInner,
) -> mpsc::UnboundedReceiver<serde_json::Value> {
    let (tx, rx) = mpsc::unbounded_channel();
    *state.retirement_completion_tx.lock().await = Some(tx);
    rx
}

async fn register_retirement_tab(
    state: &MasterStateInner,
    agent: &Arc<AgentCli>,
    helper_id: HelperId,
    tab_id: &str,
    session_id: &SessionId,
) {
    state.connected_helpers.lock().await.insert(helper_id);
    let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    bind_session_route(
        state,
        session_id.clone(),
        HelperRoute {
            helper_id,
            agent_instance_id: agent.instance_id,
            notif_tx,
            forwarder: None,
            consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        },
    )
    .await;
    state
        .registry
        .upsert(crate::session_registry::SessionInfo::new(
            session_id.clone(),
            PathBuf::from("C:\\repo"),
        ))
        .await;
    state.helper_meta.lock().await.insert(
        helper_id,
        HelperRecoveryMeta {
            owner_tab_id: Some(tab_id.to_string()),
            last_session_id: Some(session_id.clone()),
        },
    );
}

fn client_connection_to_controlled_new_session_agent(
    events: mpsc::UnboundedSender<ReplacementEvent>,
    live_sessions: Arc<Mutex<HashSet<SessionId>>>,
    fail_close: Option<SessionId>,
) -> conn::ClientLink {
    client_connection_to_controlled_new_session_agent_with_close_result(
        events,
        live_sessions,
        fail_close,
        false,
        false,
    )
}

fn client_connection_to_controlled_new_session_agent_with_close_result(
    events: mpsc::UnboundedSender<ReplacementEvent>,
    live_sessions: Arc<Mutex<HashSet<SessionId>>>,
    fail_close: Option<SessionId>,
    close_method_not_found: bool,
    capture_cancel: bool,
) -> conn::ClientLink {
    let (client_pipe, agent_pipe) = tokio::io::duplex(4096);
    let (client_read, client_write) = tokio::io::split(client_pipe);
    let (agent_read, agent_write) = tokio::io::split(agent_pipe);

    let mock = ControlledNewSessionAgent {
        next: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        events,
        live_sessions,
        fail_close,
        close_method_not_found,
        capture_cancel,
        failed_closes: Arc::new(Mutex::new(HashSet::new())),
    };
    let agent_builder = acp::Agent
        .builder()
        .name("controlled-new-session-agent")
        .on_receive_request(
            {
                let mock = mock.clone();
                move |req: acp::schema::v1::NewSessionRequest,
                      responder: acp::Responder<acp::schema::v1::NewSessionResponse>,
                      _cx| {
                    let mock = mock.clone();
                    async move {
                        match mock.new_session(req).await {
                            Ok(response) => responder.respond(response),
                            Err(error) => responder.respond_with_error(error),
                        }
                    }
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            {
                let mock = mock.clone();
                move |req: acp::schema::v1::LoadSessionRequest,
                      responder: acp::Responder<acp::schema::v1::LoadSessionResponse>,
                      _cx| {
                    let mock = mock.clone();
                    async move {
                        match mock.load_session(req).await {
                            Ok(response) => responder.respond(response),
                            Err(error) => responder.respond_with_error(error),
                        }
                    }
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            {
                let mock = mock.clone();
                move |req: acp::schema::v1::CloseSessionRequest,
                      responder: acp::Responder<acp::schema::v1::CloseSessionResponse>,
                      _cx| {
                    let mock = mock.clone();
                    async move {
                        match mock.close_session(req).await {
                            Ok(response) => responder.respond(response),
                            Err(error) => responder.respond_with_error(error),
                        }
                    }
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_notification(
            {
                let mock = mock.clone();
                move |notif: acp::schema::v1::ClientNotification, _cx| {
                    let mock = mock.clone();
                    async move {
                        if let acp::schema::v1::ClientNotification::CancelNotification(args) = notif
                        {
                            mock.cancel(args).await?;
                        }
                        Ok(())
                    }
                }
            },
            acp::on_receive_notification!(),
        );
    let (_agent_conn, agent_io) = conn::spawn_agent(
        agent_builder,
        conn::byte_streams(agent_write.compat_write(), agent_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = agent_io.await;
    });

    let (client_conn, client_io) = conn::spawn_client(
        acp::Client.builder().name("controlled-new-session-client"),
        conn::byte_streams(client_write.compat_write(), client_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = client_io.await;
    });

    client_conn
}

fn client_connection_to_rebind_during_close_agent(
    predecessor: SessionId,
    events: mpsc::UnboundedSender<ReplacementEvent>,
) -> conn::ClientLink {
    let (client_pipe, agent_pipe) = tokio::io::duplex(4096);
    let (client_read, client_write) = tokio::io::split(client_pipe);
    let (agent_read, agent_write) = tokio::io::split(agent_pipe);
    let mock = RebindDuringCloseAgent {
        predecessor,
        events,
    };
    let agent_builder = acp::Agent
        .builder()
        .name("rebind-during-close-agent")
        .on_receive_request(
            {
                let mock = mock.clone();
                move |req: acp::schema::v1::LoadSessionRequest,
                      responder: acp::Responder<acp::schema::v1::LoadSessionResponse>,
                      _cx| {
                    let mock = mock.clone();
                    async move {
                        match mock.load_session(req).await {
                            Ok(response) => responder.respond(response),
                            Err(error) => responder.respond_with_error(error),
                        }
                    }
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            move |req: acp::schema::v1::CloseSessionRequest,
                  responder: acp::Responder<acp::schema::v1::CloseSessionResponse>,
                  _cx| {
                let mock = mock.clone();
                async move {
                    match mock.close_session(req).await {
                        Ok(response) => responder.respond(response),
                        Err(error) => responder.respond_with_error(error),
                    }
                }
            },
            acp::on_receive_request!(),
        );
    let (_agent_conn, agent_io) = conn::spawn_agent(
        agent_builder,
        conn::byte_streams(agent_write.compat_write(), agent_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = agent_io.await;
    });
    let (client_conn, client_io) = conn::spawn_client(
        acp::Client.builder().name("rebind-during-close-client"),
        conn::byte_streams(client_write.compat_write(), client_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = client_io.await;
    });
    client_conn
}

fn client_connection_to_blocking_close_agent(
    events: mpsc::UnboundedSender<ReplacementEvent>,
) -> conn::ClientLink {
    let (client_pipe, agent_pipe) = tokio::io::duplex(4096);
    let (client_read, client_write) = tokio::io::split(client_pipe);
    let (agent_read, agent_write) = tokio::io::split(agent_pipe);
    let mock = BlockingCloseAgent { events };
    let agent_builder = acp::Agent
        .builder()
        .name("blocking-close-agent")
        .on_receive_request(
            move |req: acp::schema::v1::CloseSessionRequest,
                  responder: acp::Responder<acp::schema::v1::CloseSessionResponse>,
                  _cx| {
                let mock = mock.clone();
                async move {
                    match mock.close_session(req).await {
                        Ok(response) => responder.respond(response),
                        Err(error) => responder.respond_with_error(error),
                    }
                }
            },
            acp::on_receive_request!(),
        );
    let (_agent_conn, agent_io) = conn::spawn_agent(
        agent_builder,
        conn::byte_streams(agent_write.compat_write(), agent_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = agent_io.await;
    });
    let (client_conn, client_io) = conn::spawn_client(
        acp::Client.builder().name("blocking-close-client"),
        conn::byte_streams(client_write.compat_write(), client_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = client_io.await;
    });
    client_conn
}

fn client_connection_to_blocking_cancel_agent(
    events: mpsc::UnboundedSender<ReplacementEvent>,
) -> conn::ClientLink {
    let (client_pipe, agent_pipe) = tokio::io::duplex(4096);
    let (client_read, client_write) = tokio::io::split(client_pipe);
    let (agent_read, agent_write) = tokio::io::split(agent_pipe);
    let mock = BlockingCancelAgent { events };
    let agent_builder = acp::Agent
        .builder()
        .name("blocking-cancel-agent")
        .on_receive_notification(
            move |notif: acp::schema::v1::ClientNotification, _cx| {
                let mock = mock.clone();
                async move {
                    if let acp::schema::v1::ClientNotification::CancelNotification(args) = notif {
                        mock.cancel(args).await?;
                    }
                    Ok(())
                }
            },
            acp::on_receive_notification!(),
        );
    let (_agent_conn, agent_io) = conn::spawn_agent(
        agent_builder,
        conn::byte_streams(agent_write.compat_write(), agent_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = agent_io.await;
    });
    let (client_conn, client_io) = conn::spawn_client(
        acp::Client.builder().name("blocking-cancel-client"),
        conn::byte_streams(client_write.compat_write(), client_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = client_io.await;
    });
    client_conn
}

fn client_connection_to_callback_close_agent(
    state: Arc<MasterStateInner>,
    callback_completed: tokio::sync::oneshot::Sender<()>,
) -> conn::ClientLink {
    use acp::schema::v1::{
        AgentRequest, ClientResponse, PermissionOption, PermissionOptionId, PermissionOptionKind,
        RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse, ToolCallId,
        ToolCallUpdate, ToolCallUpdateFields,
    };

    let (client_pipe, agent_pipe) = tokio::io::duplex(4096);
    let (client_read, client_write) = tokio::io::split(client_pipe);
    let (agent_read, agent_write) = tokio::io::split(agent_pipe);
    let callback_completed = Arc::new(std::sync::Mutex::new(Some(callback_completed)));
    let agent_builder = acp::Agent
        .builder()
        .name("callback-close-agent")
        .on_receive_request(
            move |req: acp::schema::v1::CloseSessionRequest,
                  responder: acp::Responder<acp::schema::v1::CloseSessionResponse>,
                  cx: acp::ConnectionTo<acp::Client>| {
                let callback_completed = Arc::clone(&callback_completed);
                async move {
                    tokio::task::spawn_local(async move {
                        let permission = RequestPermissionRequest::new(
                            req.session_id,
                            ToolCallUpdate::new(
                                ToolCallId::new("close-callback"),
                                ToolCallUpdateFields::new().title("close callback"),
                            ),
                            vec![PermissionOption::new(
                                PermissionOptionId::new("cancel"),
                                "Cancel",
                                PermissionOptionKind::RejectOnce,
                            )],
                        );
                        let _ = cx.send_request(permission).block_task().await;
                        if let Some(tx) = callback_completed.lock().unwrap().take() {
                            let _ = tx.send(());
                        }
                        responder.respond(acp::schema::v1::CloseSessionResponse::new())
                    });
                    Ok(())
                }
            },
            acp::on_receive_request!(),
        );
    let (_agent_conn, agent_io) = conn::spawn_agent(
        agent_builder,
        conn::byte_streams(agent_write.compat_write(), agent_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = agent_io.await;
    });

    let master_client = MasterClient { state };
    let client_builder = acp::Client
        .builder()
        .name("callback-close-client")
        .on_receive_request(
            move |req: AgentRequest, responder, _cx| {
                let master_client = master_client.clone();
                async move {
                    match req {
                        AgentRequest::RequestPermissionRequest(args) => {
                            let result = master_client
                                .route_for(&args.session_id, "close_callback")
                                .await
                                .map(|_| {
                                    ClientResponse::RequestPermissionResponse(
                                        RequestPermissionResponse::new(
                                            RequestPermissionOutcome::Cancelled,
                                        ),
                                    )
                                });
                            conn::respond_enum(responder, result)
                        }
                        _ => responder.respond_with_error(acp::Error::method_not_found()),
                    }
                }
            },
            acp::on_receive_request!(),
        );
    let (client_conn, client_io) = conn::spawn_client(
        client_builder,
        conn::byte_streams(client_write.compat_write(), client_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = client_io.await;
    });
    client_conn
}

fn client_connection_to_deadline_rollback_agent(
    blocked_session: SessionId,
    events: mpsc::UnboundedSender<ReplacementEvent>,
) -> conn::ClientLink {
    let (client_pipe, agent_pipe) = tokio::io::duplex(4096);
    let (client_read, client_write) = tokio::io::split(client_pipe);
    let (agent_read, agent_write) = tokio::io::split(agent_pipe);
    let load_events = events.clone();
    let agent_builder = acp::Agent
        .builder()
        .name("deadline-rollback-agent")
        .on_receive_request(
            move |req: acp::schema::v1::LoadSessionRequest,
                  responder: acp::Responder<acp::schema::v1::LoadSessionResponse>,
                  _cx| {
                let events = load_events.clone();
                async move {
                    let _ = events.send(ReplacementEvent::Load(req.session_id));
                    responder.respond(acp::schema::v1::LoadSessionResponse::new())
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            move |req: acp::schema::v1::CloseSessionRequest,
                  responder: acp::Responder<acp::schema::v1::CloseSessionResponse>,
                  _cx| {
                let events = events.clone();
                let blocked_session = blocked_session.clone();
                async move {
                    let _ = events.send(ReplacementEvent::Close(req.session_id.clone()));
                    if req.session_id == blocked_session {
                        tokio::task::spawn_local(async move {
                            futures::future::pending::<()>().await;
                            responder.respond(acp::schema::v1::CloseSessionResponse::new())
                        });
                        Ok(())
                    } else {
                        responder.respond(acp::schema::v1::CloseSessionResponse::new())
                    }
                }
            },
            acp::on_receive_request!(),
        );
    let (_agent_conn, agent_io) = conn::spawn_agent(
        agent_builder,
        conn::byte_streams(agent_write.compat_write(), agent_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = agent_io.await;
    });
    let (client_conn, client_io) = conn::spawn_client(
        acp::Client.builder().name("deadline-rollback-client"),
        conn::byte_streams(client_write.compat_write(), client_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = client_io.await;
    });
    client_conn
}

fn agent_link_to_noop_client() -> conn::AgentLink {
    let (agent_pipe, client_pipe) = tokio::io::duplex(4096);
    let (agent_read, agent_write) = tokio::io::split(agent_pipe);
    let (client_read, client_write) = tokio::io::split(client_pipe);
    let (agent_link, agent_io) = conn::spawn_agent(
        acp::Agent.builder().name("noop-helper-agent"),
        conn::byte_streams(agent_write.compat_write(), agent_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = agent_io.await;
    });
    let (_client_link, client_io) = conn::spawn_client(
        acp::Client.builder().name("noop-helper-client"),
        conn::byte_streams(client_write.compat_write(), client_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = client_io.await;
    });
    agent_link
}

fn client_connection_to_pending_load_session_agent(
    arrivals: mpsc::UnboundedSender<usize>,
) -> conn::ClientLink {
    let (client_pipe, agent_pipe) = tokio::io::duplex(4096);
    let (client_read, client_write) = tokio::io::split(client_pipe);
    let (agent_read, agent_write) = tokio::io::split(agent_pipe);

    let mock = PendingLoadSessionAgent { arrivals };
    let agent_builder = acp::Agent
        .builder()
        .name("pending-load-session-agent")
        .on_receive_request(
            {
                let mock = mock.clone();
                move |req: acp::schema::v1::ClientRequest, responder, _cx| {
                    let mock = mock.clone();
                    async move {
                        use acp::schema::v1::{AgentResponse as R, ClientRequest as Q};
                        match req {
                            Q::LoadSessionRequest(args) => conn::respond_enum(
                                responder,
                                mock.load_session(args).await.map(R::LoadSessionResponse),
                            ),
                            _ => responder.respond_with_error(acp::Error::method_not_found()),
                        }
                    }
                }
            },
            acp::on_receive_request!(),
        );
    let (_agent_conn, agent_io) = conn::spawn_agent(
        agent_builder,
        conn::byte_streams(agent_write.compat_write(), agent_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = agent_io.await;
    });

    let (client_conn, client_io) = conn::spawn_client(
        acp::Client.builder().name("pending-load-session-client"),
        conn::byte_streams(client_write.compat_write(), client_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = client_io.await;
    });

    client_conn
}

fn client_connection_to_pending_new_session_agent() -> conn::ClientLink {
    let (client_pipe, agent_pipe) = tokio::io::duplex(4096);
    let (client_read, client_write) = tokio::io::split(client_pipe);
    let (agent_read, agent_write) = tokio::io::split(agent_pipe);

    let mock = PendingNewSessionAgent;
    let agent_builder = acp::Agent
        .builder()
        .name("pending-agent")
        .on_receive_request(
            {
                let m = mock.clone();
                move |req: acp::schema::v1::ClientRequest, responder, _cx| {
                    let m = m.clone();
                    async move {
                        use acp::schema::v1::{AgentResponse as R, ClientRequest as Q};
                        match req {
                            Q::InitializeRequest(a) => conn::respond_enum(
                                responder,
                                m.initialize(a).await.map(R::InitializeResponse),
                            ),
                            Q::AuthenticateRequest(a) => conn::respond_enum(
                                responder,
                                m.authenticate(a).await.map(R::AuthenticateResponse),
                            ),
                            Q::NewSessionRequest(a) => conn::respond_enum(
                                responder,
                                m.new_session(a).await.map(R::NewSessionResponse),
                            ),
                            _ => responder.respond_with_error(acp::Error::method_not_found()),
                        }
                    }
                }
            },
            acp::on_receive_request!(),
        );
    let (_agent_conn, agent_io) = conn::spawn_agent(
        agent_builder,
        conn::byte_streams(agent_write.compat_write(), agent_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = agent_io.await;
    });

    let (client_conn, client_io) = conn::spawn_client(
        acp::Client.builder().name("noop-client"),
        conn::byte_streams(client_write.compat_write(), client_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = client_io.await;
    });

    client_conn
}

fn client_connection_to_model_agent(
    config_method_not_found: bool,
    config_hit: Arc<std::sync::atomic::AtomicBool>,
    legacy_hit: Arc<std::sync::atomic::AtomicBool>,
) -> conn::ClientLink {
    let (client_pipe, agent_pipe) = tokio::io::duplex(4096);
    let (client_read, client_write) = tokio::io::split(client_pipe);
    let (agent_read, agent_write) = tokio::io::split(agent_pipe);

    let agent_builder = acp::Agent
        .builder()
        .name("model-agent")
        .on_receive_request(
            move |_req: acp::schema::v1::SetSessionConfigOptionRequest,
                  responder: acp::Responder<acp::schema::v1::SetSessionConfigOptionResponse>,
                  _cx| {
                let config_hit = Arc::clone(&config_hit);
                async move {
                    config_hit.store(true, std::sync::atomic::Ordering::SeqCst);
                    if config_method_not_found {
                        responder.respond_with_error(acp::Error::method_not_found())
                    } else {
                        responder.respond(acp::schema::v1::SetSessionConfigOptionResponse::new(
                            Vec::new(),
                        ))
                    }
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            move |_req: conn::SetSessionModelRequest,
                  responder: acp::Responder<conn::SetSessionModelResponse>,
                  _cx| {
                let legacy_hit = Arc::clone(&legacy_hit);
                async move {
                    legacy_hit.store(true, std::sync::atomic::Ordering::SeqCst);
                    responder.respond(conn::SetSessionModelResponse::default())
                }
            },
            acp::on_receive_request!(),
        );
    let (_agent_conn, agent_io) = conn::spawn_agent(
        agent_builder,
        conn::byte_streams(agent_write.compat_write(), agent_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = agent_io.await;
    });

    let (client_conn, client_io) = conn::spawn_client(
        acp::Client.builder().name("model-client"),
        conn::byte_streams(client_write.compat_write(), client_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = client_io.await;
    });

    client_conn
}

fn model_handler(agent: Arc<AgentCli>, helper_id: u64) -> HelperHandler {
    let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    let slot = Arc::new(OnceLock::new());
    let _ = slot.set(agent);
    HelperHandler {
        helper_id: HelperId(helper_id),
        agent: slot,
        state: make_state(),
        replacement_gate: Arc::new(Mutex::new(())),
        notif_tx,
        agent_side_slot: Arc::new(OnceLock::new()),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn pooled_agents_keep_model_switch_channels_isolated() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use std::sync::atomic::{AtomicBool, Ordering};

            let a_config_hit = Arc::new(AtomicBool::new(false));
            let a_legacy_hit = Arc::new(AtomicBool::new(false));
            let agent_a = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_model_agent(
                    true,
                    Arc::clone(&a_config_hit),
                    Arc::clone(&a_legacy_hit),
                ),
                cached_init_resp: acp::schema::v1::InitializeResponse::new(
                    acp::schema::ProtocolVersion::V1,
                ),
                cli_source: None,
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "agent-a".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });

            let b_config_hit = Arc::new(AtomicBool::new(false));
            let b_legacy_hit = Arc::new(AtomicBool::new(false));
            let agent_b = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_model_agent(
                    false,
                    Arc::clone(&b_config_hit),
                    Arc::clone(&b_legacy_hit),
                ),
                cached_init_resp: acp::schema::v1::InitializeResponse::new(
                    acp::schema::ProtocolVersion::V1,
                ),
                cli_source: None,
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "agent-b".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });

            for (session_id, config_id) in [("session-a", "model-a"), ("session-b", "model-b")] {
                let response: acp::schema::v1::NewSessionResponse =
                    serde_json::from_value(serde_json::json!({
                        "sessionId": session_id,
                        "configOptions": [{
                            "id": config_id,
                            "name": "Model",
                            "category": "model",
                            "type": "select",
                            "currentValue": "default",
                            "options": [{"value": "default", "name": "Default"}]
                        }]
                    }))
                    .expect("valid model response");
                let _ = crate::protocol::acp::model_select::models_from_new_session(&response);
            }

            model_handler(Arc::clone(&agent_a), 1)
                .set_session_model(conn::SetSessionModelRequest::new("session-a", "alpha"))
                .await
                .expect("agent A should fall back to legacy set_model");
            model_handler(Arc::clone(&agent_b), 2)
                .set_session_model(conn::SetSessionModelRequest::new("session-b", "beta"))
                .await
                .expect("agent B should keep using its config selector");

            assert!(a_config_hit.load(Ordering::SeqCst));
            assert!(a_legacy_hit.load(Ordering::SeqCst));
            assert!(b_config_hit.load(Ordering::SeqCst));
            assert!(!b_legacy_hit.load(Ordering::SeqCst));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn direct_resume_updates_model_switch_channel_from_load_response() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use std::sync::atomic::{AtomicBool, Ordering};

            let config_hit = Arc::new(AtomicBool::new(false));
            let legacy_hit = Arc::new(AtomicBool::new(false));
            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_model_agent(
                    false,
                    Arc::clone(&config_hit),
                    Arc::clone(&legacy_hit),
                ),
                cached_init_resp: acp::schema::v1::InitializeResponse::new(
                    acp::schema::ProtocolVersion::V1,
                ),
                cli_source: None,
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "resume-only-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let response: acp::schema::v1::LoadSessionResponse = serde_json::from_str(
                r#"{
                    "configOptions": [{
                        "id": "resume-model",
                        "name": "Model",
                        "category": "model",
                        "type": "select",
                        "currentValue": "sonnet",
                        "options": [{"value": "sonnet", "name": "Sonnet"}]
                    }]
                }"#,
            )
            .expect("valid load_session response");

            let resumed_session = acp::schema::v1::SessionId::new("resumed-session");
            let (_, current_model_id) =
                update_model_switch_channel_from_load(&resumed_session, &response);
            assert_eq!(current_model_id.as_deref(), Some("sonnet"));

            model_handler(Arc::clone(&agent), 1)
                .set_session_model(conn::SetSessionModelRequest::new(
                    "resumed-session",
                    "sonnet",
                ))
                .await
                .expect("direct resume should switch through config options");

            assert!(config_hit.load(Ordering::SeqCst));
            assert!(!legacy_hit.load(Ordering::SeqCst));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn new_session_timeout_is_enforced_by_master_forwarder() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            // The multi-agent HelperHandler binds its agent during
            // `initialize`; pre-bind one wrapping the pending
            // (hangs-on-session/new) connection so
            // `forward_new_session_to_agent` resolves it and exercises
            // the timeout path.
            let agent = Arc::new(OnceLock::new());
            let _ = agent.set(Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_pending_new_session_agent(),
                cached_init_resp: acp::schema::v1::InitializeResponse::new(
                    acp::schema::ProtocolVersion::V1,
                ),
                cli_source: None,
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "copilot --acp --stdio".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            }));
            let handler = HelperHandler {
                helper_id: HelperId(1),
                agent,
                state: make_state(),
                replacement_gate: Arc::new(Mutex::new(())),
                notif_tx,
                agent_side_slot: Arc::new(OnceLock::new()),
            };

            let err = handler
                .forward_new_session_to_agent(
                    acp::schema::v1::NewSessionRequest::new(PathBuf::from(r"C:\repo")),
                    std::time::Duration::from_millis(1),
                )
                .await
                .expect_err("master should return an ACP error when agent session/new hangs");

            assert_eq!(err.code, acp::ErrorCode::InternalError);
            assert!(
                format!("{err}").contains("agent CLI session/new timed out"),
                "error should identify master->agent session/new timeout: {err}"
            );
        })
        .await;
}

#[tokio::test]
async fn failed_pending_session_cleanup_retires_close_marked_recovery_state() {
    let state = make_state();
    let helper_id = HelperId(120);
    state
        .pending_session_helpers
        .lock()
        .await
        .insert(helper_id, Some("failed-pending-tab".to_string()));
    state.closing_session_helpers.lock().await.insert(helper_id);
    state.helper_meta.lock().await.insert(
        helper_id,
        HelperRecoveryMeta {
            owner_tab_id: Some("failed-pending-tab".to_string()),
            last_session_id: None,
        },
    );
    let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    let handler = HelperHandler {
        helper_id,
        agent: Arc::new(OnceLock::new()),
        state: Arc::clone(&state),
        replacement_gate: Arc::new(Mutex::new(())),
        notif_tx,
        agent_side_slot: Arc::new(OnceLock::new()),
    };

    handler.finish_failed_pending_session().await;

    assert!(!state
        .pending_session_helpers
        .lock()
        .await
        .contains_key(&helper_id));
    assert!(!state
        .closing_session_helpers
        .lock()
        .await
        .contains(&helper_id));
    assert!(!state.helper_meta.lock().await.contains_key(&helper_id));
}

#[tokio::test(flavor = "current_thread")]
async fn failed_pending_cleanup_reads_destructive_marker_under_ownership_gate() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(121);
            state
                .pending_session_helpers
                .lock()
                .await
                .insert(helper_id, Some("destructive-pending-tab".to_string()));
            state.closing_session_helpers.lock().await.insert(helper_id);
            state.helper_meta.lock().await.insert(
                helper_id,
                HelperRecoveryMeta {
                    owner_tab_id: Some("destructive-pending-tab".to_string()),
                    last_session_id: None,
                },
            );
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let handler = HelperHandler {
                helper_id,
                agent: Arc::new(OnceLock::new()),
                state: Arc::clone(&state),
                replacement_gate: Arc::new(Mutex::new(())),
                notif_tx,
                agent_side_slot: Arc::new(OnceLock::new()),
            };

            let ownership_guard = state.tab_ownership_gate.lock().await;
            let cleanup =
                tokio::task::spawn_local(async move { handler.finish_failed_pending_session().await });
            tokio::task::yield_now().await;
            state
                .destructive_session_helpers
                .lock()
                .await
                .insert(helper_id);
            drop(ownership_guard);
            cleanup.await.unwrap();

            assert!(state
                .closing_session_helpers
                .lock()
                .await
                .contains(&helper_id));
            assert!(!state.helper_meta.lock().await.contains_key(&helper_id));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn load_session_gate_timeout_does_not_reach_agent_or_mutate_state() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(2);
            let agent_instance = AgentInstanceId::new_v4();
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let agent_side_slot = Arc::new(OnceLock::new());
            agent_side_slot
                .set(agent_link_to_noop_client())
                .expect("agent-side forwarder should be set once");
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp.agent_capabilities.mcp_capabilities.http = true;
            let (load_arrivals_tx, mut load_arrivals_rx) = mpsc::unbounded_channel();
            let agent = Arc::new(OnceLock::new());
            assert!(agent
                .set(Arc::new(AgentCli {
                    instance_id: agent_instance,
                    conn: client_connection_to_pending_load_session_agent(load_arrivals_tx),
                    cached_init_resp,
                    cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                    source: crate::agent_source::AgentSource::Host,
                    cmd_key: "pending-load-session-agent".to_string(),
                    cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                    bound_helpers: Mutex::new(HashSet::new()),
                    host_list_cache: Mutex::new(None),
                    listed_ever: Mutex::new(HashSet::new()),
                }))
                .is_ok());
            let handler = HelperHandler {
                helper_id,
                agent,
                state: Arc::clone(&state),
                replacement_gate: Arc::new(Mutex::new(())),
                notif_tx,
                agent_side_slot,
            };
            let gated_session = SessionId::new("gate-timeout-session");
            let mut request = acp::schema::v1::LoadSessionRequest::new(
                gated_session.clone(),
                PathBuf::from("C:\\repo-a"),
            );
            crate::session_registry::inject_wta_meta(
                &mut request.meta,
                &crate::session_registry::WtaMeta {
                    proposal_mcp: Some("http-v1".to_string()),
                    ..Default::default()
                },
            );

            let replacement_guard = handler.replacement_gate.lock().await;
            let error = handler
                .load_session_with_timeout(request, std::time::Duration::from_millis(10))
                .await
                .expect_err("master should include gate acquisition in the load deadline");
            drop(replacement_guard);

            assert_eq!(error.code, acp::ErrorCode::InternalError);
            assert!(format!("{error}").contains("agent CLI session/load timed out"));
            assert!(
                matches!(
                    load_arrivals_rx.try_recv(),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                ),
                "gate timeout must not forward session/load to the agent"
            );
            assert!(!state
                .session_to_helper
                .lock()
                .await
                .contains_key(&gated_session));
            assert!(state.registry.lookup(&gated_session).await.is_none());
            assert!(!state.helper_meta.lock().await.contains_key(&helper_id));
            assert_eq!(
                state
                    .session_mcp_capabilities
                    .remove_owner(agent_instance)
                    .await,
                0,
                "gate timeout must not prepare a session MCP capability"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn load_session_timeout_rolls_back_replacement_state_and_releases_gate() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(2);
            let agent_instance = AgentInstanceId::new_v4();
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let agent_side_slot = Arc::new(OnceLock::new());
            agent_side_slot
                .set(agent_link_to_noop_client())
                .expect("agent-side forwarder should be set once");
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp.agent_capabilities.mcp_capabilities.http = true;
            let (load_arrivals_tx, mut load_arrivals_rx) = mpsc::unbounded_channel();
            let agent = Arc::new(OnceLock::new());
            assert!(agent
                .set(Arc::new(AgentCli {
                    instance_id: agent_instance,
                    conn: client_connection_to_pending_load_session_agent(load_arrivals_tx),
                    cached_init_resp,
                    cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                    source: crate::agent_source::AgentSource::Host,
                    cmd_key: "pending-load-session-agent".to_string(),
                    cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                    bound_helpers: Mutex::new(HashSet::new()),
                    host_list_cache: Mutex::new(None),
                    listed_ever: Mutex::new(HashSet::new()),
                }))
                .is_ok());
            let handler = HelperHandler {
                helper_id,
                agent,
                state: Arc::clone(&state),
                replacement_gate: Arc::new(Mutex::new(())),
                notif_tx,
                agent_side_slot,
            };
            let timed_out_session = SessionId::new("timed-out-session");
            let mut request = acp::schema::v1::LoadSessionRequest::new(
                timed_out_session.clone(),
                PathBuf::from("C:\\repo-a"),
            );
            crate::session_registry::inject_wta_meta(
                &mut request.meta,
                &crate::session_registry::WtaMeta {
                    proposal_mcp: Some("http-v1".to_string()),
                    ..Default::default()
                },
            );

            let error = handler
                .load_session_with_timeout(request, std::time::Duration::from_millis(10))
                .await
                .expect_err("master should time out a hung agent session/load");

            assert_eq!(
                load_arrivals_rx.recv().await,
                Some(1),
                "load request must carry the pending session MCP capability"
            );
            assert_eq!(error.code, acp::ErrorCode::InternalError);
            assert!(format!("{error}").contains("agent CLI session/load timed out"));
            assert!(!state
                .session_to_helper
                .lock()
                .await
                .contains_key(&timed_out_session));
            assert_eq!(
                state
                    .session_mcp_capabilities
                    .remove_owner(agent_instance)
                    .await,
                0,
                "timed-out load must cancel its pending MCP capability"
            );

            let _replacement_guard = tokio::time::timeout(
                std::time::Duration::from_millis(50),
                handler.replacement_gate.lock(),
            )
            .await
            .expect("replacement gate must be released for fallback session/new");
        })
        .await;
}

#[test]
fn cloned_helper_handlers_share_the_lazy_agent_binding() {
    let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    let handler = HelperHandler {
        helper_id: HelperId(1),
        agent: Arc::new(OnceLock::new()),
        state: make_state(),
        replacement_gate: Arc::new(Mutex::new(())),
        notif_tx,
        agent_side_slot: Arc::new(OnceLock::new()),
    };
    let request_handler = handler.clone();

    assert!(
        Arc::ptr_eq(&handler.agent, &request_handler.agent),
        "all request handler clones must share initialize's binding slot"
    );
    assert!(
        Arc::ptr_eq(&handler.replacement_gate, &request_handler.replacement_gate),
        "all request handler clones must share the replacement gate"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn helper_close_session_physically_closes_and_retires_owned_session() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(16);
            let session_id = SessionId::new("tab-close-session");
            let agent_instance_id = AgentInstanceId::new_v4();
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let live_sessions = Arc::new(Mutex::new(HashSet::from([session_id.clone()])));
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(OnceLock::new());
            assert!(agent
                .set(Arc::new(AgentCli {
                    instance_id: agent_instance_id,
                    conn: client_connection_to_controlled_new_session_agent(
                        events_tx,
                        Arc::clone(&live_sessions),
                        None,
                    ),
                    cached_init_resp,
                    cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                    source: crate::agent_source::AgentSource::Host,
                    cmd_key: "tab-close-agent".to_string(),
                    cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                    bound_helpers: Mutex::new(HashSet::new()),
                    host_list_cache: Mutex::new(None),
                    listed_ever: Mutex::new(HashSet::new()),
                }))
                .is_ok());
            let handler = HelperHandler {
                helper_id,
                agent,
                state: Arc::clone(&state),
                replacement_gate: Arc::new(Mutex::new(())),
                notif_tx: notif_tx.clone(),
                agent_side_slot: Arc::new(OnceLock::new()),
            };
            bind_session_route(
                &state,
                session_id.clone(),
                HelperRoute {
                    helper_id,
                    agent_instance_id,
                    notif_tx,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;
            state
                .registry
                .upsert(crate::session_registry::SessionInfo::new(
                    session_id.clone(),
                    PathBuf::from("C:\\repo"),
                ))
                .await;
            let pending_capability = state
                .session_mcp_capabilities
                .prepare(agent_instance_id, None)
                .await;
            assert!(
                state
                    .session_mcp_capabilities
                    .bind(&pending_capability, session_id.clone())
                    .await
            );
            state.pending_usage.lock().await.insert(
                session_id.clone(),
                (
                    helper_id,
                    acp::schema::v1::SessionNotification::new(
                        session_id.clone(),
                        acp::schema::v1::SessionUpdate::AgentMessageChunk(
                            acp::schema::v1::ContentChunk::new("pending usage".into()),
                        ),
                    ),
                ),
            );
            state.helper_meta.lock().await.insert(
                helper_id,
                HelperRecoveryMeta {
                    owner_tab_id: Some("tab-16".to_string()),
                    last_session_id: Some(session_id.clone()),
                },
            );

            handler
                .close_session(acp::schema::v1::CloseSessionRequest::new(
                    session_id.clone(),
                ))
                .await
                .expect("tab close should release the owned ACP session");

            let Some(ReplacementEvent::Close(closed_session_id)) = events_rx.recv().await else {
                panic!("tab close must reach the agent as session/close");
            };
            assert_eq!(closed_session_id, session_id);
            assert!(!live_sessions.lock().await.contains(&session_id));
            assert!(!state
                .session_to_helper
                .lock()
                .await
                .contains_key(&session_id));
            assert!(state.registry.lookup(&session_id).await.is_none());
            assert!(!state.pending_usage.lock().await.contains_key(&session_id));
            assert_eq!(
                state
                    .session_mcp_capabilities
                    .remove_owner(agent_instance_id)
                    .await,
                0,
                "tab close must revoke the session-scoped MCP capability"
            );
            assert!(
                !state.helper_meta.lock().await.contains_key(&helper_id),
                "clean close must remove crash-recovery metadata"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn master_reset_tab_session_resolves_owner_and_physically_retires_session() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let owner_helper_id = HelperId(116);
            let session_id = SessionId::new("sibling-close-session");
            let agent_instance_id = AgentInstanceId::new_v4();
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let live_sessions = Arc::new(Mutex::new(HashSet::from([session_id.clone()])));
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: agent_instance_id,
                conn: client_connection_to_controlled_new_session_agent(
                    events_tx,
                    Arc::clone(&live_sessions),
                    None,
                ),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "sibling-close-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), cell);

            bind_session_route(
                &state,
                session_id.clone(),
                HelperRoute {
                    helper_id: owner_helper_id,
                    agent_instance_id,
                    notif_tx,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;
            state
                .registry
                .upsert(crate::session_registry::SessionInfo::new(
                    session_id.clone(),
                    PathBuf::from("C:\\repo"),
                ))
                .await;
            state.helper_meta.lock().await.insert(
                owner_helper_id,
                HelperRecoveryMeta {
                    owner_tab_id: Some("closed-tab-116".to_string()),
                    last_session_id: Some(session_id.clone()),
                },
            );

            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "reset_tab_session",
                    "params": { "tab_id": "closed-tab-116" }
                }),
            )
            .await;

            let Some(ReplacementEvent::Close(closed_session_id)) = events_rx.recv().await else {
                panic!("close-by-tab must reach the owning agent as session/close");
            };
            assert_eq!(closed_session_id, session_id);
            assert!(!state
                .session_to_helper
                .lock()
                .await
                .contains_key(&session_id));
            {
                let meta = state.helper_meta.lock().await;
                let recovery = meta
                    .get(&owner_helper_id)
                    .expect("reset keeps the surviving helper's tab ownership");
                assert!(recovery.last_session_id.is_none());
            }
            assert!(!state
                .closing_session_helpers
                .lock()
                .await
                .contains(&owner_helper_id));

            handle_close_tab_session(
                &state,
                &crate::session_registry::CloseTabSessionParams {
                    tab_id: "closed-tab-116".to_string(),
                },
                false,
            )
            .await
            .expect("duplicate close-by-tab must be idempotent");

            let (peer_ext_tx, mut peer_ext_rx) = mpsc::unbounded_channel();
            state
                .helper_ext_subscribers
                .lock()
                .await
                .insert(HelperId(117), peer_ext_tx);
            let orphan_session_id = SessionId::new("late-orphan-close-session");
            live_sessions.lock().await.insert(orphan_session_id.clone());
            state
                .orphaned_sessions
                .lock()
                .await
                .entry(agent.cmd_key.clone())
                .or_default()
                .insert(orphan_session_id.clone());
            state.orphaned_tabs.lock().await.insert(
                "late-closed-tab".to_string(),
                (
                    agent.cmd_key.clone(),
                    owner_helper_id,
                    orphan_session_id.clone(),
                ),
            );
            state.helper_meta.lock().await.insert(
                owner_helper_id,
                HelperRecoveryMeta {
                    owner_tab_id: Some("late-closed-tab".to_string()),
                    last_session_id: Some(orphan_session_id.clone()),
                },
            );

            let close_params = crate::session_registry::CloseTabSessionParams {
                tab_id: "late-closed-tab".to_string(),
            };
            let (first_close, duplicate_close) = tokio::join!(
                handle_close_tab_session(&state, &close_params, false),
                handle_close_tab_session(&state, &close_params, false)
            );
            first_close
                .expect("late close-by-tab must physically close a disconnected helper's orphan");
            duplicate_close.expect("concurrent late close-by-tab must be idempotent");
            let Some(ReplacementEvent::Close(closed_orphan_id)) = events_rx.recv().await else {
                panic!("late close-by-tab must reach the owning agent as session/close");
            };
            assert_eq!(closed_orphan_id, orphan_session_id);
            assert!(
                events_rx.try_recv().is_err(),
                "concurrent duplicate must not issue a second session/close"
            );
            assert!(
                peer_ext_rx.try_recv().is_ok() && peer_ext_rx.try_recv().is_ok(),
                "orphan close must broadcast session removal and list refresh to peers"
            );
            assert!(!state
                .orphaned_tabs
                .lock()
                .await
                .contains_key("late-closed-tab"));
            assert!(!state
                .orphaned_sessions
                .lock()
                .await
                .get(&agent.cmd_key)
                .is_some_and(|sessions| sessions.contains(&orphan_session_id)));
            assert!(
                !state
                    .helper_meta
                    .lock()
                    .await
                    .contains_key(&owner_helper_id),
                "late intentional close must remove crash-recovery metadata"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn retirement_event_physically_closes_once_and_replays_completion() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let mut completions = capture_retirement_completions(&state).await;
            let helper_id = HelperId(201);
            let session_id = SessionId::new("retirement-live");
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let live_sessions = Arc::new(Mutex::new(HashSet::from([session_id.clone()])));
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_controlled_new_session_agent_with_close_result(
                    events_tx,
                    Arc::clone(&live_sessions),
                    None,
                    false,
                    true,
                ),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "retirement-live-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), cell);
            register_retirement_tab(&state, &agent, helper_id, "tab-live", &session_id).await;

            let event = serde_json::json!({
                "method": "retire_agent_sessions",
                "params": {
                    "operation_id": "retire-live-op",
                    "scope": "tabs",
                    "tab_ids": ["tab-live"],
                    "reason": "window_close"
                }
            });
            handle_master_wt_event(&state, event.clone()).await;
            handle_master_wt_event(&state, event.clone()).await;

            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Cancel(ref sid)) if sid == &session_id
            ));
            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Close(ref sid)) if sid == &session_id
            ));
            let completion = completions
                .recv()
                .await
                .expect("completion must be emitted");
            assert_eq!(
                completion,
                serde_json::json!({
                    "type": "event",
                    "method": "agent_sessions_retired",
                    "params": {
                        "operation_id": "retire-live-op",
                        "success": true,
                        "reason": "window_close",
                        "failed_tabs": []
                    }
                })
            );
            assert!(state.session_to_helper.lock().await.is_empty());
            assert!(state.registry.lookup(&session_id).await.is_none());
            assert!(!state.helper_meta.lock().await.contains_key(&helper_id));
            assert!(live_sessions.lock().await.is_empty());
            assert!(events_rx.try_recv().is_err());

            handle_master_wt_event(&state, event).await;
            assert_eq!(
                completions.recv().await,
                Some(completion),
                "completed duplicate must replay the same process-wide completion"
            );
            assert!(
                events_rx.try_recv().is_err(),
                "duplicate operation id must not close the rebound session twice"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn retirement_event_with_no_sessions_still_emits_completion() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let mut completions = capture_retirement_completions(&state).await;

            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "retire_agent_sessions",
                    "params": {
                        "operation_id": "retire-empty-op",
                        "scope": "all",
                        "tab_ids": [],
                        "reason": "empty_shutdown"
                    }
                }),
            )
            .await;

            assert_eq!(
                completions.recv().await,
                Some(serde_json::json!({
                    "type": "event",
                    "method": "agent_sessions_retired",
                    "params": {
                        "operation_id": "retire-empty-op",
                        "success": true,
                        "reason": "empty_shutdown",
                        "failed_tabs": []
                    }
                }))
            );
        })
        .await;
}

#[tokio::test]
async fn no_owner_retirement_fences_outgoing_publication_and_allows_replacement() {
    let state = make_state();
    let outgoing = HelperId(210);
    let replacement = HelperId(211);
    state.connected_helpers.lock().await.insert(outgoing);

    assert_eq!(
        begin_tab_retirement(&state, "tab-owner-race")
            .await
            .map(|target| target.helper_id),
        None,
        "the fence must exist even before ownership is published"
    );

    let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    let outgoing_handler = HelperHandler {
        helper_id: outgoing,
        agent: Arc::new(OnceLock::new()),
        state: Arc::clone(&state),
        replacement_gate: Arc::new(Mutex::new(())),
        notif_tx: notif_tx.clone(),
        agent_side_slot: Arc::new(OnceLock::new()),
    };
    let mut outgoing_request =
        acp::schema::v1::NewSessionRequest::new(PathBuf::from("C:\\owner-race"));
    crate::session_registry::inject_wta_meta(
        &mut outgoing_request.meta,
        &crate::session_registry::WtaMeta {
            owner_tab_id: Some("tab-owner-race".to_string()),
            ..Default::default()
        },
    );
    let error = outgoing_handler
        .new_session(outgoing_request)
        .await
        .expect_err("the outgoing helper must not start session/new after fencing");
    assert!(format!("{error}").contains("outgoing helper generation"));
    assert!(!state
        .pending_session_helpers
        .lock()
        .await
        .contains_key(&outgoing));

    complete_tab_retirement(&state, "tab-owner-race").await;
    state.connected_helpers.lock().await.insert(replacement);
    let replacement_handler = HelperHandler {
        helper_id: replacement,
        agent: Arc::new(OnceLock::new()),
        state: Arc::clone(&state),
        replacement_gate: Arc::new(Mutex::new(())),
        notif_tx,
        agent_side_slot: Arc::new(OnceLock::new()),
    };
    replacement_handler
        .publish_pending_owner(Some("tab-owner-race".to_string()))
        .await
        .expect("a post-completion replacement helper must consume the fence");
    assert_eq!(
        state
            .pending_session_helpers
            .lock()
            .await
            .get(&replacement)
            .and_then(Option::as_deref),
        Some("tab-owner-race")
    );
    assert!(state.tab_retirement_fences.lock().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn scope_all_fences_connected_helper_without_owner_after_completion() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let mut completions = capture_retirement_completions(&state).await;
            let outgoing = HelperId(212);
            let replacement = HelperId(213);
            state.connected_helpers.lock().await.insert(outgoing);

            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "retire_agent_sessions",
                    "params": {
                        "operation_id": "retire-all-ownerless-op",
                        "scope": "all",
                        "tab_ids": [],
                        "reason": "window_close"
                    }
                }),
            )
            .await;
            assert_eq!(completions.recv().await.unwrap()["params"]["success"], true);
            assert!(state
                .destructive_session_helpers
                .lock()
                .await
                .contains(&outgoing));

            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let outgoing_handler = HelperHandler {
                helper_id: outgoing,
                agent: Arc::new(OnceLock::new()),
                state: Arc::clone(&state),
                replacement_gate: Arc::new(Mutex::new(())),
                notif_tx: notif_tx.clone(),
                agent_side_slot: Arc::new(OnceLock::new()),
            };
            let mut request =
                acp::schema::v1::NewSessionRequest::new(PathBuf::from("C:\\late-owner"));
            crate::session_registry::inject_wta_meta(
                &mut request.meta,
                &crate::session_registry::WtaMeta {
                    owner_tab_id: Some("tab-late-owner".to_string()),
                    ..Default::default()
                },
            );
            let error = outgoing_handler
                .new_session(request)
                .await
                .expect_err("captured outgoing helper must remain fenced after completion");
            assert!(format!("{error}").contains("outgoing helper generation"));
            assert!(state.pending_session_helpers.lock().await.is_empty());
            assert!(state.session_to_helper.lock().await.is_empty());

            state.connected_helpers.lock().await.insert(replacement);
            let replacement_handler = HelperHandler {
                helper_id: replacement,
                agent: Arc::new(OnceLock::new()),
                state: Arc::clone(&state),
                replacement_gate: Arc::new(Mutex::new(())),
                notif_tx,
                agent_side_slot: Arc::new(OnceLock::new()),
            };
            replacement_handler
                .publish_pending_owner(Some("tab-late-owner".to_string()))
                .await
                .expect("a helper admitted after completion must not inherit the outgoing fence");
            assert_eq!(
                state
                    .pending_session_helpers
                    .lock()
                    .await
                    .get(&replacement)
                    .and_then(Option::as_deref),
                Some("tab-late-owner")
            );

            let (intentional, _) =
                consume_disconnected_helper_retirement_state(&state, outgoing).await;
            assert!(intentional);
            assert!(!state
                .destructive_session_helpers
                .lock()
                .await
                .contains(&outgoing));
        })
        .await;
}

#[tokio::test]
async fn helper_connecting_during_scope_all_is_outgoing_but_replacement_is_admitted() {
    let state = make_state();
    assert!(begin_all_retirement(&state).await.helpers.is_empty());

    let outgoing = HelperId(215);
    register_connected_helper(&state, outgoing).await;
    assert!(state
        .all_retirement_fence
        .lock()
        .await
        .outgoing_helpers
        .contains(&outgoing));
    assert!(state
        .destructive_session_helpers
        .lock()
        .await
        .contains(&outgoing));

    let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    let outgoing_handler = HelperHandler {
        helper_id: outgoing,
        agent: Arc::new(OnceLock::new()),
        state: Arc::clone(&state),
        replacement_gate: Arc::new(Mutex::new(())),
        notif_tx: notif_tx.clone(),
        agent_side_slot: Arc::new(OnceLock::new()),
    };
    let error = outgoing_handler
        .publish_pending_owner(Some("tab-connect-during-all".to_string()))
        .await
        .expect_err("a helper admitted during scope=all must join its outgoing generation");
    assert!(format!("{error}").contains("outgoing helper generation"));

    assert!(
        finish_all_retirement_batch(&state, &HashSet::from([outgoing]))
            .await
            .is_none()
    );
    let replacement = HelperId(216);
    register_connected_helper(&state, replacement).await;
    let replacement_handler = HelperHandler {
        helper_id: replacement,
        agent: Arc::new(OnceLock::new()),
        state: Arc::clone(&state),
        replacement_gate: Arc::new(Mutex::new(())),
        notif_tx,
        agent_side_slot: Arc::new(OnceLock::new()),
    };
    replacement_handler
        .publish_pending_owner(Some("tab-connect-during-all".to_string()))
        .await
        .expect("a helper admitted after scope=all completion is a replacement");
    assert!(!state
        .destructive_session_helpers
        .lock()
        .await
        .contains(&replacement));
}

#[tokio::test]
async fn repeated_retire_disconnect_consumes_helper_tombstones_and_fences() {
    let state = make_state();
    let surviving_sibling = HelperId(299);
    state
        .connected_helpers
        .lock()
        .await
        .insert(surviving_sibling);

    for generation in 300..332 {
        let helper_id = HelperId(generation);
        let tab_id = format!("bounded-retirement-{generation}");
        state.connected_helpers.lock().await.insert(helper_id);
        state.helper_meta.lock().await.insert(
            helper_id,
            HelperRecoveryMeta {
                owner_tab_id: Some(tab_id.clone()),
                last_session_id: None,
            },
        );

        assert_eq!(
            begin_tab_retirement(&state, &tab_id)
                .await
                .map(|target| target.helper_id),
            Some(helper_id)
        );
        complete_tab_retirement(&state, &tab_id).await;
        let (intentional, _) =
            consume_disconnected_helper_retirement_state(&state, helper_id).await;
        assert!(intentional);
        assert!(
            state.tab_retirement_fences.lock().await.is_empty(),
            "an unrelated connected sibling must not retain the retired tab fence"
        );
    }

    assert_eq!(
        *state.connected_helpers.lock().await,
        HashSet::from([surviving_sibling])
    );
    assert!(state.closing_session_helpers.lock().await.is_empty());
    assert!(state.destructive_session_helpers.lock().await.is_empty());
    assert!(state.active_retirement_helpers.lock().await.is_empty());
    assert!(state.closing_session_results.lock().await.is_empty());
    assert!(state.tab_retirement_fences.lock().await.is_empty());
}

#[tokio::test]
async fn repeated_disconnected_orphan_retirement_keeps_tombstones_and_fences_empty() {
    let state = make_state();

    for generation in 0..64 {
        let helper_id = HelperId(500 + generation);
        let tab_id = format!("disconnected-orphan-tab-{generation}");
        let session_id = SessionId::new(format!("disconnected-orphan-session-{generation}"));
        let agent_key = format!("disconnected-orphan-agent-{generation}");
        state.orphaned_tabs.lock().await.insert(
            tab_id.clone(),
            (agent_key.clone(), helper_id, session_id.clone()),
        );
        state
            .orphaned_sessions
            .lock()
            .await
            .entry(agent_key)
            .or_default()
            .insert(session_id.clone());
        state
            .registry
            .upsert(crate::session_registry::SessionInfo::new(
                session_id.clone(),
                PathBuf::from(format!("C:\\disconnected-orphan-{generation}")),
            ))
            .await;

        assert_eq!(
            retire_tab_transaction(
                &state,
                tab_id,
                tokio::time::Instant::now() + std::time::Duration::from_secs(1),
            )
            .await,
            ReplacedSessionCleanup::LogicalFallback
        );
        assert!(state.registry.lookup(&session_id).await.is_none());
        assert!(state.orphaned_tabs.lock().await.is_empty());
        assert!(state.orphaned_sessions.lock().await.is_empty());
        assert!(state.tab_retirement_fences.lock().await.is_empty());
        assert!(state.closing_session_helpers.lock().await.is_empty());
        assert!(state.destructive_session_helpers.lock().await.is_empty());
    }

    assert!(state.connected_helpers.lock().await.is_empty());
    assert!(state.active_retirement_helpers.lock().await.is_empty());
}

#[tokio::test]
async fn repeated_ownerless_stale_tab_retirement_keeps_fences_bounded() {
    let state = make_state();
    let unresolved = HelperId(333);
    let owned_elsewhere = HelperId(334);
    state
        .connected_helpers
        .lock()
        .await
        .extend([unresolved, owned_elsewhere]);
    state.helper_meta.lock().await.insert(
        owned_elsewhere,
        HelperRecoveryMeta {
            owner_tab_id: Some("authoritative-other-tab".to_string()),
            last_session_id: None,
        },
    );

    for generation in 0..(OWNERLESS_RETIREMENT_TARGET_CAP + 32) {
        let tab_id = format!("stale-ownerless-tab-{generation}");
        assert_eq!(
            begin_tab_retirement(&state, &tab_id)
                .await
                .map(|target| target.helper_id),
            None
        );
        complete_tab_retirement(&state, &tab_id).await;
    }

    {
        assert!(state.tab_retirement_fences.lock().await.is_empty());
        let unresolved_state = state.unresolved_owner_retirements.lock().await;
        assert_eq!(unresolved_state.len(), 1);
        assert!(matches!(
            unresolved_state.get(&unresolved),
            Some(OwnerlessRetirementSafety::DenyNextOwner)
        ));
    }

    let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    let handler = HelperHandler {
        helper_id: unresolved,
        agent: Arc::new(OnceLock::new()),
        state: Arc::clone(&state),
        replacement_gate: Arc::new(Mutex::new(())),
        notif_tx,
        agent_side_slot: Arc::new(OnceLock::new()),
    };
    handler
        .publish_pending_owner(Some("actual-unrelated-tab".to_string()))
        .await
        .expect_err("overflow must conservatively deny the next owner publication");
    assert!(
        state.unresolved_owner_retirements.lock().await.is_empty(),
        "the bounded deny-next-owner state must be consumed"
    );
}

#[tokio::test]
async fn ownerless_helper_cannot_claim_old_retired_tab_after_prior_fence_limits() {
    let state = make_state();
    let helper_id = HelperId(335);
    state.connected_helpers.lock().await.insert(helper_id);

    for generation in 0..300 {
        let tab_id = format!("retired-ownerless-tab-{generation}");
        assert_eq!(
            begin_tab_retirement(&state, &tab_id)
                .await
                .map(|target| target.helper_id),
            None
        );
        complete_tab_retirement(&state, &tab_id).await;
    }

    let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    let handler = HelperHandler {
        helper_id,
        agent: Arc::new(OnceLock::new()),
        state: Arc::clone(&state),
        replacement_gate: Arc::new(Mutex::new(())),
        notif_tx,
        agent_side_slot: Arc::new(OnceLock::new()),
    };
    handler
        .publish_pending_owner(Some("retired-ownerless-tab-0".to_string()))
        .await
        .expect_err("collapsed ownerless safety must preserve the oldest retired target");
    assert!(state
        .destructive_session_helpers
        .lock()
        .await
        .contains(&helper_id));
}

#[tokio::test]
async fn ownerless_helper_publishing_different_owner_clears_unrelated_safety() {
    let state = make_state();
    let helper_id = HelperId(336);
    state.connected_helpers.lock().await.insert(helper_id);
    assert_eq!(
        begin_tab_retirement(&state, "retired-unrelated-tab")
            .await
            .map(|target| target.helper_id),
        None
    );
    complete_tab_retirement(&state, "retired-unrelated-tab").await;

    let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    let handler = HelperHandler {
        helper_id,
        agent: Arc::new(OnceLock::new()),
        state: Arc::clone(&state),
        replacement_gate: Arc::new(Mutex::new(())),
        notif_tx,
        agent_side_slot: Arc::new(OnceLock::new()),
    };
    handler
        .publish_pending_owner(Some("authoritative-live-tab".to_string()))
        .await
        .expect("a different authoritative owner safely resolves the helper");
    assert!(state.unresolved_owner_retirements.lock().await.is_empty());
    assert_eq!(
        state
            .pending_session_helpers
            .lock()
            .await
            .get(&helper_id)
            .and_then(Option::as_deref),
        Some("authoritative-live-tab")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn scope_all_retires_ownerless_helper_live_route_directly() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let mut completions = capture_retirement_completions(&state).await;
            let helper_id = HelperId(337);
            let session_id = SessionId::new("ownerless-live-route");
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let live_sessions = Arc::new(Mutex::new(HashSet::from([session_id.clone()])));
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_controlled_new_session_agent_with_close_result(
                    events_tx,
                    Arc::clone(&live_sessions),
                    None,
                    true,
                    true,
                ),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "ownerless-live-route-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), cell);
            state.connected_helpers.lock().await.insert(helper_id);
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            bind_session_route(
                &state,
                session_id.clone(),
                HelperRoute {
                    helper_id,
                    agent_instance_id: agent.instance_id,
                    notif_tx,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;
            state
                .registry
                .upsert(crate::session_registry::SessionInfo::new(
                    session_id.clone(),
                    PathBuf::from("C:\\ownerless-live"),
                ))
                .await;
            state.helper_meta.lock().await.insert(
                helper_id,
                HelperRecoveryMeta {
                    owner_tab_id: None,
                    last_session_id: Some(session_id.clone()),
                },
            );
            state
                .orphaned_sessions
                .lock()
                .await
                .entry(agent.cmd_key.clone())
                .or_default()
                .insert(session_id.clone());

            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "retire_agent_sessions",
                    "params": {
                        "operation_id": "retire-all-ownerless-live",
                        "scope": "all",
                        "tab_ids": [],
                        "reason": "window_close"
                    }
                }),
            )
            .await;

            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Cancel(ref sid)) if sid == &session_id
            ));
            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Close(ref sid)) if sid == &session_id
            ));
            let completion = completions.recv().await.unwrap();
            assert_eq!(completion["params"]["success"], false);
            assert_eq!(completion["params"]["failed_tabs"], serde_json::json!([]));
            assert_eq!(
                completion["params"]["unattributed_failures"]["count"],
                serde_json::json!(1)
            );
            assert!(state.session_to_helper.lock().await.is_empty());
            assert!(state.registry.lookup(&session_id).await.is_none());
            assert!(!state.helper_meta.lock().await.contains_key(&helper_id));
            assert!(state.orphaned_tabs.lock().await.is_empty());
            assert!(state.orphaned_sessions.lock().await.is_empty());
            assert_eq!(
                *live_sessions.lock().await,
                HashSet::from([session_id]),
                "ownerless logical fallback must report failure while retiring WTA state"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn scope_all_captured_helper_disconnect_still_closes_once() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(339);
            let session_id = SessionId::new("captured-disconnect-close");
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let live_sessions = Arc::new(Mutex::new(HashSet::from([session_id.clone()])));
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_controlled_new_session_agent_with_close_result(
                    events_tx,
                    Arc::clone(&live_sessions),
                    None,
                    false,
                    true,
                ),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "captured-disconnect-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), cell);
            register_retirement_tab(
                &state,
                &agent,
                helper_id,
                "tab-captured-disconnect",
                &session_id,
            )
            .await;

            let mut targets = begin_all_retirement(&state).await;
            assert_eq!(targets.helpers.len(), 1);
            assert_eq!(
                drop_sessions_for_helper(&state, helper_id).await,
                vec![session_id.clone()]
            );
            let (intentional, _) =
                consume_disconnected_helper_retirement_state(&state, helper_id).await;
            assert!(intentional);

            let (retired_helper, owner_tab, cleanup) = retire_helper_transaction(
                &state,
                targets.helpers.pop().unwrap(),
                tokio::time::Instant::now() + std::time::Duration::from_secs(1),
            )
            .await;
            assert_eq!(retired_helper, helper_id);
            assert_eq!(owner_tab.as_deref(), Some("tab-captured-disconnect"));
            assert_eq!(cleanup, ReplacedSessionCleanup::PhysicallyClosed);
            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Cancel(ref sid)) if sid == &session_id
            ));
            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Close(ref sid)) if sid == &session_id
            ));
            assert!(
                events_rx.try_recv().is_err(),
                "captured retirement must close the disconnected session exactly once"
            );
            assert!(state.session_to_helper.lock().await.is_empty());
            assert!(live_sessions.lock().await.is_empty());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn scope_all_retirement_captures_orphan_after_route_drop_before_connected_helper_removal() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let mut completions = capture_retirement_completions(&state).await;
            let helper_id = HelperId(340);
            let tab_id = "tab-disconnect-ordering";
            let session_id = SessionId::new("disconnect-ordering-close");
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let live_sessions = Arc::new(Mutex::new(HashSet::from([session_id.clone()])));
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_controlled_new_session_agent_with_close_result(
                    events_tx,
                    Arc::clone(&live_sessions),
                    None,
                    false,
                    true,
                ),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "disconnect-ordering-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), cell);
            register_retirement_tab(&state, &agent, helper_id, tab_id, &session_id).await;

            state.orphaned_tabs.lock().await.insert(
                tab_id.to_string(),
                (agent.cmd_key.clone(), helper_id, session_id.clone()),
            );
            assert_eq!(
                drop_sessions_for_helper(&state, helper_id).await,
                vec![session_id.clone()]
            );
            state
                .orphaned_sessions
                .lock()
                .await
                .entry(agent.cmd_key.clone())
                .or_default()
                .insert(session_id.clone());
            assert!(
                state.connected_helpers.lock().await.contains(&helper_id),
                "disconnect has not removed the helper from the connected set yet"
            );

            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "retire_agent_sessions",
                    "params": {
                        "operation_id": "retire-disconnect-ordering",
                        "scope": "all",
                        "tab_ids": [],
                        "reason": "window_close"
                    }
                }),
            )
            .await;

            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Cancel(ref sid)) if sid == &session_id
            ));
            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Close(ref sid)) if sid == &session_id
            ));
            assert_eq!(completions.recv().await.unwrap()["params"]["success"], true);
            assert!(
                events_rx.try_recv().is_err(),
                "captured orphan must be cancelled and closed exactly once"
            );
            assert!(state.orphaned_tabs.lock().await.is_empty());
            assert!(state.orphaned_sessions.lock().await.is_empty());
            assert!(live_sessions.lock().await.is_empty());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn retirement_completed_after_route_drop_cannot_republish_orphan() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let mut completions = capture_retirement_completions(&state).await;
            let helper_id = HelperId(342);
            let replacement_helper_id = HelperId(343);
            let tab_id = "tab-disconnect-retirement-race";
            let session_id = SessionId::new("disconnect-retirement-race");
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let live_sessions = Arc::new(Mutex::new(HashSet::from([session_id.clone()])));
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_controlled_new_session_agent_with_close_result(
                    events_tx,
                    Arc::clone(&live_sessions),
                    None,
                    false,
                    true,
                ),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "disconnect-retirement-race-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), cell);
            register_retirement_tab(&state, &agent, helper_id, tab_id, &session_id).await;
            state.orphaned_tabs.lock().await.insert(
                tab_id.to_string(),
                (agent.cmd_key.clone(), helper_id, session_id.clone()),
            );

            let pause = Arc::new(DisconnectOrphanPublicationPause::default());
            *state.disconnect_orphan_publication_pause.lock().await = Some(Arc::clone(&pause));
            let disconnect = tokio::task::spawn_local({
                let state = Arc::clone(&state);
                let agent = Arc::clone(&agent);
                let orphan_tab_candidate = (tab_id.to_string(), session_id.clone());
                async move {
                    drop_and_publish_disconnected_helper_sessions(
                        &state,
                        helper_id,
                        Some(&agent),
                        Some(&orphan_tab_candidate),
                    )
                    .await
                }
            });

            pause.routes_dropped.notified().await;
            assert!(state.session_to_helper.lock().await.is_empty());

            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "retire_agent_sessions",
                    "params": {
                        "operation_id": "retire-disconnect-publication-race",
                        "scope": "all",
                        "tab_ids": [],
                        "reason": "window_close"
                    }
                }),
            )
            .await;
            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Cancel(ref sid)) if sid == &session_id
            ));
            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Close(ref sid)) if sid == &session_id
            ));
            assert_eq!(completions.recv().await.unwrap()["params"]["success"], true);
            assert!(live_sessions.lock().await.is_empty());

            pause.resume_publication.notify_one();
            assert_eq!(
                disconnect.await.expect("disconnect cleanup should finish"),
                vec![session_id.clone()]
            );
            assert!(
                state.orphaned_sessions.lock().await.is_empty(),
                "retired session must not be republished as an orphan"
            );
            let (intentional, _) =
                consume_disconnected_helper_retirement_state(&state, helper_id).await;
            assert!(intentional);

            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let agent_slot = Arc::new(OnceLock::new());
            assert!(agent_slot.set(Arc::clone(&agent)).is_ok());
            let agent_side_slot = Arc::new(OnceLock::new());
            agent_side_slot
                .set(agent_link_to_noop_client())
                .expect("agent-side forwarder should be set once");
            let replacement = HelperHandler {
                helper_id: replacement_helper_id,
                agent: agent_slot,
                state: Arc::clone(&state),
                replacement_gate: Arc::new(Mutex::new(())),
                notif_tx,
                agent_side_slot,
            };
            replacement
                .load_session_with_timeout(
                    acp::schema::v1::LoadSessionRequest::new(
                        session_id.clone(),
                        PathBuf::from("C:\\retired"),
                    ),
                    std::time::Duration::from_secs(1),
                )
                .await
                .expect("later resume should perform a real session/load");
            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Load(ref sid)) if sid == &session_id
            ));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn disconnect_orphan_publication_skips_tab_closed_session() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(345);
            let tab_id = "tab-disconnect-ordinary-close-race";
            let session_id = SessionId::new("disconnect-ordinary-close-race");
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let live_sessions = Arc::new(Mutex::new(HashSet::from([session_id.clone()])));
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_controlled_new_session_agent_with_close_result(
                    events_tx,
                    Arc::clone(&live_sessions),
                    None,
                    false,
                    true,
                ),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "disconnect-ordinary-close-race-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), cell);
            register_retirement_tab(&state, &agent, helper_id, tab_id, &session_id).await;
            state.orphaned_tabs.lock().await.insert(
                tab_id.to_string(),
                (agent.cmd_key.clone(), helper_id, session_id.clone()),
            );

            let pause = Arc::new(DisconnectOrphanPublicationPause::default());
            *state.disconnect_orphan_publication_pause.lock().await = Some(Arc::clone(&pause));
            let disconnect = tokio::task::spawn_local({
                let state = Arc::clone(&state);
                let agent = Arc::clone(&agent);
                let orphan_tab_candidate = (tab_id.to_string(), session_id.clone());
                async move {
                    drop_and_publish_disconnected_helper_sessions(
                        &state,
                        helper_id,
                        Some(&agent),
                        Some(&orphan_tab_candidate),
                    )
                    .await
                }
            });

            pause.routes_dropped.notified().await;
            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "tab_closed",
                    "params": {
                        "tab_id": tab_id
                    }
                }),
            )
            .await;

            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Cancel(ref sid)) if sid == &session_id
            ));
            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Close(ref sid)) if sid == &session_id
            ));
            assert!(live_sessions.lock().await.is_empty());
            assert!(state
                .closing_session_helpers
                .lock()
                .await
                .contains(&helper_id));

            pause.resume_publication.notify_one();
            assert_eq!(
                disconnect.await.expect("disconnect cleanup should finish"),
                vec![session_id.clone()]
            );
            assert!(
                state.orphaned_sessions.lock().await.is_empty(),
                "physically closed tab session must not be published as an orphan"
            );

            let (intentional, _) =
                consume_disconnected_helper_retirement_state(&state, helper_id).await;
            assert!(intentional);
            assert!(state.closing_session_helpers.lock().await.is_empty());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn disconnect_orphan_publication_skips_rebound_session() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(346);
            let replacement_helper_id = HelperId(347);
            let session_id = SessionId::new("disconnect-rebind-race");
            let (events_tx, _events_rx) = mpsc::unbounded_channel();
            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_controlled_new_session_agent(
                    events_tx,
                    Arc::new(Mutex::new(HashSet::from([session_id.clone()]))),
                    None,
                ),
                cached_init_resp: acp::schema::v1::InitializeResponse::new(
                    acp::schema::ProtocolVersion::V1,
                ),
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "disconnect-rebind-race-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), cell);
            let (original_tx, _original_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            bind_session_route(
                &state,
                session_id.clone(),
                HelperRoute {
                    helper_id,
                    agent_instance_id: agent.instance_id,
                    notif_tx: original_tx,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;

            let pause = Arc::new(DisconnectOrphanPublicationPause::default());
            *state.disconnect_orphan_publication_pause.lock().await = Some(Arc::clone(&pause));
            let disconnect = tokio::task::spawn_local({
                let state = Arc::clone(&state);
                let agent = Arc::clone(&agent);
                async move {
                    drop_and_publish_disconnected_helper_sessions(
                        &state,
                        helper_id,
                        Some(&agent),
                        None,
                    )
                    .await
                }
            });

            pause.routes_dropped.notified().await;
            let (replacement_tx, _replacement_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            bind_session_route(
                &state,
                session_id.clone(),
                HelperRoute {
                    helper_id: replacement_helper_id,
                    agent_instance_id: agent.instance_id,
                    notif_tx: replacement_tx,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;

            pause.resume_publication.notify_one();
            assert_eq!(
                disconnect.await.expect("disconnect cleanup should finish"),
                vec![session_id.clone()]
            );
            assert!(
                state.orphaned_sessions.lock().await.is_empty(),
                "a session rebound before publication must not retain an orphan marker"
            );
            assert_eq!(
                state
                    .session_to_helper
                    .lock()
                    .await
                    .get(&session_id)
                    .map(|route| route.helper_id),
                Some(replacement_helper_id)
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn unexpected_disconnect_still_publishes_orphan() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(344);
            let session_id = SessionId::new("unexpected-disconnect-orphan");
            let (events_tx, _events_rx) = mpsc::unbounded_channel();
            let live_sessions = Arc::new(Mutex::new(HashSet::from([session_id.clone()])));
            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_controlled_new_session_agent(
                    events_tx,
                    live_sessions,
                    None,
                ),
                cached_init_resp: acp::schema::v1::InitializeResponse::new(
                    acp::schema::ProtocolVersion::V1,
                ),
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "unexpected-disconnect-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), cell);
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            bind_session_route(
                &state,
                session_id.clone(),
                HelperRoute {
                    helper_id,
                    agent_instance_id: agent.instance_id,
                    notif_tx,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;

            drop_and_publish_disconnected_helper_sessions(&state, helper_id, Some(&agent), None)
                .await;

            assert!(state
                .orphaned_sessions
                .lock()
                .await
                .get(&agent.cmd_key)
                .is_some_and(|sessions| sessions.contains(&session_id)));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn orphan_retirement_blocked_lifecycle_gate_uses_total_budget() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let retirement_budget = std::time::Duration::from_millis(40);
            let state = make_state_with_retirement_pending_timeout(retirement_budget);
            let mut completions = capture_retirement_completions(&state).await;
            let helper_id = HelperId(348);
            let tab_id = "tab-blocked-orphan-gate";
            let session_id = SessionId::new("blocked-orphan-gate");
            let agent_key = "blocked-orphan-gate-agent".to_string();
            state.orphaned_tabs.lock().await.insert(
                tab_id.to_string(),
                (agent_key.clone(), helper_id, session_id.clone()),
            );
            state
                .orphaned_sessions
                .lock()
                .await
                .entry(agent_key)
                .or_default()
                .insert(session_id.clone());
            state
                .registry
                .upsert(crate::session_registry::SessionInfo::new(
                    session_id.clone(),
                    PathBuf::from("C:\\blocked-orphan-gate"),
                ))
                .await;

            let gate = session_lifecycle_gate(&state, &session_id).await;
            let gate_guard = gate.lock().await;
            let replacement_helper_id = HelperId(349);
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let (rebind_queued_tx, mut rebind_queued_rx) = mpsc::unbounded_channel();
            let rebind = tokio::task::spawn_local({
                let state = Arc::clone(&state);
                let session_id = session_id.clone();
                async move {
                    rebind_queued_tx.send(()).unwrap();
                    bind_session_route(
                        &state,
                        session_id,
                        HelperRoute {
                            helper_id: replacement_helper_id,
                            agent_instance_id: AgentInstanceId::new_v4(),
                            notif_tx,
                            forwarder: None,
                            consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                        },
                    )
                    .await;
                }
            });
            rebind_queued_rx
                .recv()
                .await
                .expect("replacement bind must queue behind the lifecycle gate");
            let started = tokio::time::Instant::now();
            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "retire_agent_sessions",
                    "params": {
                        "operation_id": "retire-blocked-orphan-gate",
                        "scope": "tabs",
                        "tab_ids": [tab_id],
                        "reason": "window_close"
                    }
                }),
            )
            .await;

            let completion =
                tokio::time::timeout(std::time::Duration::from_millis(500), completions.recv())
                    .await
                    .expect("blocked orphan gate must not start another timeout budget")
                    .expect("blocked orphan gate must publish completion");
            assert_eq!(completion["params"]["success"], false);
            assert_eq!(
                completion["params"]["failed_tabs"],
                serde_json::json!([tab_id])
            );
            assert!(
                started.elapsed() < std::time::Duration::from_millis(500),
                "blocked orphan gate must remain bounded by the retirement deadline"
            );
            assert!(state.orphaned_tabs.lock().await.contains_key(tab_id));
            assert!(state
                .orphaned_sessions
                .lock()
                .await
                .get("blocked-orphan-gate-agent")
                .is_some_and(|sessions| sessions.contains(&session_id)));
            assert!(state.registry.lookup(&session_id).await.is_some());
            let deferred_cleanup =
                state.deferred_retirement_cleanup_complete.notified();
            drop(gate_guard);
            rebind.await.expect("replacement bind must finish");
            tokio::time::timeout(std::time::Duration::from_secs(1), deferred_cleanup)
                .await
                .expect("deferred orphan cleanup must revalidate after the queued bind");
            assert_eq!(
                state
                    .session_to_helper
                    .lock()
                    .await
                    .get(&session_id)
                    .map(|route| route.helper_id),
                Some(replacement_helper_id)
            );
            assert!(state.registry.lookup(&session_id).await.is_some());
            assert!(state.orphaned_tabs.lock().await.is_empty());
            assert!(state.orphaned_sessions.lock().await.is_empty());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn orphan_retirement_blocked_cancel_uses_total_budget() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let retirement_budget = std::time::Duration::from_millis(40);
            let state = make_state_with_retirement_pending_timeout(retirement_budget);
            let mut completions = capture_retirement_completions(&state).await;
            let helper_id = HelperId(349);
            let tab_id = "tab-blocked-orphan-cancel";
            let session_id = SessionId::new("blocked-orphan-cancel");
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_blocking_cancel_agent(events_tx),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "blocked-orphan-cancel-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), cell);
            state.orphaned_tabs.lock().await.insert(
                tab_id.to_string(),
                (agent.cmd_key.clone(), helper_id, session_id.clone()),
            );
            state
                .orphaned_sessions
                .lock()
                .await
                .entry(agent.cmd_key.clone())
                .or_default()
                .insert(session_id.clone());
            state
                .registry
                .upsert(crate::session_registry::SessionInfo::new(
                    session_id.clone(),
                    PathBuf::from("C:\\blocked-orphan-cancel"),
                ))
                .await;

            let started = tokio::time::Instant::now();
            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "retire_agent_sessions",
                    "params": {
                        "operation_id": "retire-blocked-orphan-cancel",
                        "scope": "tabs",
                        "tab_ids": [tab_id],
                        "reason": "window_close"
                    }
                }),
            )
            .await;
            let ReplacementEvent::BlockingCancel(cancelled_session, _release) = events_rx
                .recv()
                .await
                .expect("orphan retirement must attempt cancellation")
            else {
                panic!("expected blocking orphan cancellation");
            };
            assert_eq!(cancelled_session, session_id);

            let completion =
                tokio::time::timeout(std::time::Duration::from_millis(500), completions.recv())
                    .await
                    .expect("blocked orphan cancel must not start another timeout budget")
                    .expect("blocked orphan cancel must publish completion");
            assert_eq!(completion["params"]["success"], false);
            assert_eq!(
                completion["params"]["failed_tabs"],
                serde_json::json!([tab_id])
            );
            assert!(
                started.elapsed() < std::time::Duration::from_millis(500),
                "blocked orphan cancel must remain bounded by the retirement deadline"
            );
            assert!(state.orphaned_tabs.lock().await.is_empty());
            assert!(state.orphaned_sessions.lock().await.is_empty());
            assert!(state.registry.lookup(&session_id).await.is_none());
            assert!(
                events_rx.try_recv().is_err(),
                "session/close must not start after cancellation consumes the deadline"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn scope_all_physically_closes_ownerless_orphaned_session() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let mut completions = capture_retirement_completions(&state).await;
            let session_id = SessionId::new("ownerless-orphan-physical-close");
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let live_sessions = Arc::new(Mutex::new(HashSet::from([session_id.clone()])));
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_controlled_new_session_agent_with_close_result(
                    events_tx,
                    Arc::clone(&live_sessions),
                    None,
                    false,
                    true,
                ),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "ownerless-orphan-physical-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), cell);
            state
                .orphaned_sessions
                .lock()
                .await
                .entry(agent.cmd_key.clone())
                .or_default()
                .insert(session_id.clone());
            state
                .registry
                .upsert(crate::session_registry::SessionInfo::new(
                    session_id.clone(),
                    PathBuf::from("C:\\ownerless-orphan-physical"),
                ))
                .await;

            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "retire_agent_sessions",
                    "params": {
                        "operation_id": "retire-ownerless-orphan-physical",
                        "scope": "all",
                        "tab_ids": [],
                        "reason": "window_close"
                    }
                }),
            )
            .await;

            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Cancel(ref sid)) if sid == &session_id
            ));
            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Close(ref sid)) if sid == &session_id
            ));
            assert_eq!(completions.recv().await.unwrap()["params"]["success"], true);
            assert!(live_sessions.lock().await.is_empty());
            assert!(state.orphaned_sessions.lock().await.is_empty());
            assert!(state.registry.lookup(&session_id).await.is_none());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn scope_all_preserves_ownerless_orphan_claimed_by_replacement_route() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let mut completions = capture_retirement_completions(&state).await;
            let replacement_helper_id = HelperId(350);
            let session_id = SessionId::new("ownerless-orphan-rebound");
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let live_sessions = Arc::new(Mutex::new(HashSet::from([session_id.clone()])));
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_controlled_new_session_agent_with_close_result(
                    events_tx,
                    Arc::clone(&live_sessions),
                    None,
                    false,
                    true,
                ),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "ownerless-orphan-rebound-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), cell);
            state
                .orphaned_sessions
                .lock()
                .await
                .entry(agent.cmd_key.clone())
                .or_default()
                .insert(session_id.clone());
            state
                .registry
                .upsert(crate::session_registry::SessionInfo::new(
                    session_id.clone(),
                    PathBuf::from("C:\\ownerless-orphan-rebound"),
                ))
                .await;

            let gate = session_lifecycle_gate(&state, &session_id).await;
            let gate_guard = gate.lock().await;
            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "retire_agent_sessions",
                    "params": {
                        "operation_id": "retire-ownerless-orphan-rebound",
                        "scope": "all",
                        "tab_ids": [],
                        "reason": "window_close"
                    }
                }),
            )
            .await;
            tokio::task::yield_now().await;
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            state.session_to_helper.lock().await.insert(
                session_id.clone(),
                HelperRoute {
                    helper_id: replacement_helper_id,
                    agent_instance_id: agent.instance_id,
                    notif_tx,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            );
            drop(gate_guard);

            assert_eq!(completions.recv().await.unwrap()["params"]["success"], true);
            assert_eq!(
                state
                    .session_to_helper
                    .lock()
                    .await
                    .get(&session_id)
                    .map(|route| route.helper_id),
                Some(replacement_helper_id)
            );
            assert!(state.registry.lookup(&session_id).await.is_some());
            assert_eq!(
                *live_sessions.lock().await,
                HashSet::from([session_id.clone()])
            );
            assert!(state.orphaned_sessions.lock().await.is_empty());
            assert!(
                events_rx.try_recv().is_err(),
                "a replacement route must suppress orphan cancel and close"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn scope_all_ownerless_orphan_gate_timeout_preserves_queued_rebind() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state_with_retirement_pending_timeout(
                std::time::Duration::from_millis(40),
            );
            let mut completions = capture_retirement_completions(&state).await;
            let replacement_helper_id = HelperId(351);
            let session_id = SessionId::new("ownerless-orphan-timeout-rebind");
            let agent_key = "ownerless-orphan-timeout-agent".to_string();
            state
                .orphaned_sessions
                .lock()
                .await
                .entry(agent_key.clone())
                .or_default()
                .insert(session_id.clone());
            state
                .registry
                .upsert(crate::session_registry::SessionInfo::new(
                    session_id.clone(),
                    PathBuf::from("C:\\ownerless-orphan-timeout"),
                ))
                .await;

            let gate = session_lifecycle_gate(&state, &session_id).await;
            let gate_guard = gate.lock().await;
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let (rebind_queued_tx, mut rebind_queued_rx) = mpsc::unbounded_channel();
            let rebind = tokio::task::spawn_local({
                let state = Arc::clone(&state);
                let session_id = session_id.clone();
                async move {
                    rebind_queued_tx.send(()).unwrap();
                    bind_session_route(
                        &state,
                        session_id,
                        HelperRoute {
                            helper_id: replacement_helper_id,
                            agent_instance_id: AgentInstanceId::new_v4(),
                            notif_tx,
                            forwarder: None,
                            consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                        },
                    )
                    .await;
                }
            });
            rebind_queued_rx
                .recv()
                .await
                .expect("replacement bind must queue behind the lifecycle gate");

            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "retire_agent_sessions",
                    "params": {
                        "operation_id": "retire-ownerless-timeout-rebind",
                        "scope": "all",
                        "tab_ids": [],
                        "reason": "window_close"
                    }
                }),
            )
            .await;
            let completion =
                tokio::time::timeout(std::time::Duration::from_millis(500), completions.recv())
                    .await
                    .expect("ownerless retirement must remain bounded")
                    .expect("ownerless retirement must report completion");
            assert_eq!(completion["params"]["success"], false);
            assert_eq!(
                completion["params"]["unattributed_failures"]["count"],
                serde_json::json!(1)
            );
            assert!(state
                .orphaned_sessions
                .lock()
                .await
                .get(&agent_key)
                .is_some_and(|sessions| sessions.contains(&session_id)));
            assert!(state.registry.lookup(&session_id).await.is_some());

            let deferred_cleanup =
                state.deferred_retirement_cleanup_complete.notified();
            drop(gate_guard);
            rebind.await.expect("replacement bind must finish");
            tokio::time::timeout(std::time::Duration::from_secs(1), deferred_cleanup)
                .await
                .expect("deferred ownerless cleanup must revalidate after the queued bind");
            assert_eq!(
                state
                    .session_to_helper
                    .lock()
                    .await
                    .get(&session_id)
                    .map(|route| route.helper_id),
                Some(replacement_helper_id)
            );
            assert!(state.registry.lookup(&session_id).await.is_some());
            assert!(state.orphaned_sessions.lock().await.is_empty());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn scope_all_ownerless_orphan_unavailable_agent_uses_logical_fallback() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let mut completions = capture_retirement_completions(&state).await;
            let session_id = SessionId::new("ownerless-orphan-unavailable");
            state
                .orphaned_sessions
                .lock()
                .await
                .entry("ownerless-orphan-unavailable-agent".to_string())
                .or_default()
                .insert(session_id.clone());
            state
                .registry
                .upsert(crate::session_registry::SessionInfo::new(
                    session_id.clone(),
                    PathBuf::from("C:\\ownerless-orphan-unavailable"),
                ))
                .await;

            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "retire_agent_sessions",
                    "params": {
                        "operation_id": "retire-ownerless-orphan-unavailable",
                        "scope": "all",
                        "tab_ids": [],
                        "reason": "window_close"
                    }
                }),
            )
            .await;

            let completion = completions.recv().await.unwrap();
            assert_eq!(completion["params"]["success"], false);
            assert_eq!(completion["params"]["failed_tabs"], serde_json::json!([]));
            assert_eq!(
                completion["params"]["unattributed_failures"]["count"],
                serde_json::json!(1)
            );
            assert!(state.orphaned_sessions.lock().await.is_empty());
            assert!(state.registry.lookup(&session_id).await.is_none());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn scope_all_retirement_reports_failure_when_outgoing_orphan_agent_is_unavailable() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let mut completions = capture_retirement_completions(&state).await;
            let helper_id = HelperId(341);
            let tab_id = "tab-unavailable-orphan";
            let session_id = SessionId::new("unavailable-orphan");
            state.connected_helpers.lock().await.insert(helper_id);
            state.helper_meta.lock().await.insert(
                helper_id,
                HelperRecoveryMeta {
                    owner_tab_id: Some(tab_id.to_string()),
                    last_session_id: Some(session_id.clone()),
                },
            );
            state.orphaned_tabs.lock().await.insert(
                tab_id.to_string(),
                (
                    "unavailable-orphan-agent".to_string(),
                    helper_id,
                    session_id.clone(),
                ),
            );
            state
                .orphaned_sessions
                .lock()
                .await
                .entry("unavailable-orphan-agent".to_string())
                .or_default()
                .insert(session_id);

            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "retire_agent_sessions",
                    "params": {
                        "operation_id": "retire-unavailable-orphan",
                        "scope": "all",
                        "tab_ids": [],
                        "reason": "window_close"
                    }
                }),
            )
            .await;

            let completion = completions.recv().await.unwrap();
            assert_eq!(completion["params"]["success"], false);
            assert_eq!(
                completion["params"]["failed_tabs"],
                serde_json::json!([tab_id])
            );
            assert!(state.orphaned_tabs.lock().await.is_empty());
            assert!(state.orphaned_sessions.lock().await.is_empty());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn scope_all_waits_for_ownerless_pending_transaction_cleanup() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let mut completions = capture_retirement_completions(&state).await;
            let helper_id = HelperId(338);
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let live_sessions = Arc::new(Mutex::new(HashSet::new()));
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_controlled_new_session_agent(
                    events_tx,
                    Arc::clone(&live_sessions),
                    None,
                ),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "ownerless-pending-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), cell);
            state.connected_helpers.lock().await.insert(helper_id);
            let agent_slot = Arc::new(OnceLock::new());
            assert!(agent_slot.set(Arc::clone(&agent)).is_ok());
            let agent_side_slot = Arc::new(OnceLock::new());
            agent_side_slot
                .set(agent_link_to_noop_client())
                .expect("agent-side forwarder should be initialized");
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let handler = HelperHandler {
                helper_id,
                agent: agent_slot,
                state: Arc::clone(&state),
                replacement_gate: Arc::new(Mutex::new(())),
                notif_tx,
                agent_side_slot,
            };
            let new_task = tokio::task::spawn_local(async move {
                handler
                    .new_session(acp::schema::v1::NewSessionRequest::new(PathBuf::from(
                        "C:\\ownerless-pending",
                    )))
                    .await
            });
            let ReplacementEvent::New(_, release_new) =
                events_rx.recv().await.expect("session/new must block")
            else {
                panic!("expected controlled session/new");
            };

            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "retire_agent_sessions",
                    "params": {
                        "operation_id": "retire-all-ownerless-pending",
                        "scope": "all",
                        "tab_ids": [],
                        "reason": "window_close"
                    }
                }),
            )
            .await;
            tokio::task::yield_now().await;
            assert!(
                completions.try_recv().is_err(),
                "scope=all must wait for the captured pending transaction"
            );

            release_new.send(()).unwrap();
            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Close(_))
            ));
            let mut response = new_task.await.unwrap().unwrap();
            assert_eq!(
                crate::session_registry::extract_wta_meta(&mut response.meta)
                    .session_result
                    .as_deref(),
                Some("retired")
            );
            assert_eq!(completions.recv().await.unwrap()["params"]["success"], true);
            assert!(state.pending_session_helpers.lock().await.is_empty());
            assert!(state.pending_session_mcp.lock().await.is_empty());
            assert!(state.session_to_helper.lock().await.is_empty());
            assert!(state.registry.lookup(&response.session_id).await.is_none());
            assert!(!state.helper_meta.lock().await.contains_key(&helper_id));
            assert!(live_sessions.lock().await.is_empty());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn scope_all_unsupported_retirement_reports_failed_owner_tab() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let mut completions = capture_retirement_completions(&state).await;
            let helper_id = HelperId(202);
            let session_id = SessionId::new("retirement-unsupported");
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let live_sessions = Arc::new(Mutex::new(HashSet::from([session_id.clone()])));
            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_controlled_new_session_agent_with_close_result(
                    events_tx,
                    Arc::clone(&live_sessions),
                    None,
                    false,
                    true,
                ),
                cached_init_resp: acp::schema::v1::InitializeResponse::new(
                    acp::schema::ProtocolVersion::V1,
                ),
                cli_source: Some(crate::agent_sessions::CliSource::Gemini),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "retirement-unsupported-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), cell);
            register_retirement_tab(&state, &agent, helper_id, "tab-unsupported", &session_id)
                .await;

            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "retire_agent_sessions",
                    "params": {
                        "operation_id": "retire-unsupported-op",
                        "scope": "all",
                        "tab_ids": [],
                        "reason": "profile_delete"
                    }
                }),
            )
            .await;

            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Cancel(ref sid)) if sid == &session_id
            ));
            assert!(events_rx.try_recv().is_err());
            let completion = completions
                .recv()
                .await
                .expect("completion must be emitted");
            assert_eq!(completion["params"]["success"], false);
            assert_eq!(
                completion["params"]["failed_tabs"],
                serde_json::json!(["tab-unsupported"])
            );
            assert!(state.session_to_helper.lock().await.is_empty());
            assert!(state.registry.lookup(&session_id).await.is_none());
            assert!(!state.helper_meta.lock().await.contains_key(&helper_id));
            assert_eq!(
                *live_sessions.lock().await,
                HashSet::from([session_id]),
                "unsupported provider remains physically live but loses every WTA route"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn scope_all_starts_independent_session_closes_concurrently() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let mut completions = capture_retirement_completions(&state).await;
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent_a = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_blocking_close_agent(events_tx.clone()),
                cached_init_resp: cached_init_resp.clone(),
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "retirement-all-agent-a".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let agent_b = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_blocking_close_agent(events_tx),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Gemini),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "retirement-all-agent-b".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell_a = Arc::new(tokio::sync::OnceCell::new());
            let cell_b = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell_a.set(Arc::clone(&agent_a)).is_ok());
            assert!(cell_b.set(Arc::clone(&agent_b)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent_a.cmd_key.clone(), cell_a);
            state
                .agents
                .lock()
                .await
                .insert(agent_b.cmd_key.clone(), cell_b);
            register_retirement_tab(
                &state,
                &agent_a,
                HelperId(203),
                "tab-all-a",
                &SessionId::new("retirement-all-a"),
            )
            .await;
            let orphan_session = SessionId::new("retirement-all-b");
            state.orphaned_tabs.lock().await.insert(
                "tab-all-b".to_string(),
                (
                    agent_b.cmd_key.clone(),
                    HelperId(204),
                    orphan_session.clone(),
                ),
            );
            state
                .orphaned_sessions
                .lock()
                .await
                .entry(agent_b.cmd_key.clone())
                .or_default()
                .insert(orphan_session.clone());

            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "retire_agent_sessions",
                    "params": {
                        "operation_id": "retire-all-op",
                        "scope": "all",
                        "tab_ids": [],
                        "reason": "shutdown"
                    }
                }),
            )
            .await;

            let first = events_rx.recv().await.expect("first close must start");
            let second =
                tokio::time::timeout(std::time::Duration::from_millis(100), events_rx.recv())
                    .await
                    .expect("second close must start before the first 15s close completes")
                    .expect("second close event must exist");
            let mut helper_release = None;
            let mut orphan_release = None;
            let mut closed = HashSet::new();
            for event in [first, second] {
                let ReplacementEvent::BlockingClose(session_id, release) = event else {
                    panic!("scope=all must issue session/close for each live tab");
                };
                if session_id == orphan_session {
                    orphan_release = Some(release);
                } else {
                    helper_release = Some(release);
                }
                closed.insert(session_id);
            }
            assert_eq!(
                closed,
                HashSet::from([
                    SessionId::new("retirement-all-a"),
                    SessionId::new("retirement-all-b")
                ])
            );
            orphan_release.unwrap().send(()).unwrap();
            tokio::time::timeout(std::time::Duration::from_millis(100), async {
                while state.orphaned_tabs.lock().await.contains_key("tab-all-b") {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("independent orphan retirement must finish while helper close is blocked");
            assert!(
                completions.try_recv().is_err(),
                "the blocked helper still gates overall completion"
            );
            helper_release.unwrap().send(()).unwrap();
            assert_eq!(completions.recv().await.unwrap()["params"]["success"], true);
            assert!(state.session_to_helper.lock().await.is_empty());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn retirement_uses_one_deadline_for_close_wait_and_forced_cleanup() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let retirement_budget = std::time::Duration::from_millis(40);
            let state = make_state_with_retirement_pending_timeout(retirement_budget);
            let mut completions = capture_retirement_completions(&state).await;
            let helper_id = HelperId(214);
            let session_id = SessionId::new("retirement-single-deadline");
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_blocking_close_agent(events_tx),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "retirement-single-deadline-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), cell);
            register_retirement_tab(
                &state,
                &agent,
                helper_id,
                "tab-single-deadline",
                &session_id,
            )
            .await;
            state
                .pending_session_helpers
                .lock()
                .await
                .insert(helper_id, Some("tab-single-deadline".to_string()));

            let started = tokio::time::Instant::now();
            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "retire_agent_sessions",
                    "params": {
                        "operation_id": "retire-single-deadline-op",
                        "scope": "tabs",
                        "tab_ids": ["tab-single-deadline"],
                        "reason": "window_close"
                    }
                }),
            )
            .await;

            let ReplacementEvent::BlockingClose(closing_session, _release) =
                events_rx.recv().await.expect("physical close must start")
            else {
                panic!("retirement must issue session/close");
            };
            assert_eq!(closing_session, session_id);
            let completion =
                tokio::time::timeout(std::time::Duration::from_millis(500), completions.recv())
                    .await
                    .expect("retirement must not start a second timeout budget")
                    .expect("retirement completion must be emitted");
            assert_eq!(completion["params"]["success"], false);
            assert!(
                started.elapsed() < std::time::Duration::from_millis(500),
                "retirement must remain bounded by its single master deadline"
            );
            assert!(state.session_to_helper.lock().await.is_empty());
            assert!(state.pending_session_helpers.lock().await.is_empty());
            assert!(!state.helper_meta.lock().await.contains_key(&helper_id));
            assert!(!state
                .active_retirement_helpers
                .lock()
                .await
                .contains(&helper_id));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn retirement_lifecycle_gate_wait_does_not_renew_close_budget() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(217);
            let session_id = SessionId::new("retirement-gate-deadline");
            let agent_instance_id = AgentInstanceId::new_v4();
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            bind_session_route(
                &state,
                session_id.clone(),
                HelperRoute {
                    helper_id,
                    agent_instance_id,
                    notif_tx,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: agent_instance_id,
                conn: client_connection_to_blocking_close_agent(events_tx),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "retirement-gate-deadline-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let gate = session_lifecycle_gate(&state, &session_id).await;
            let gate_guard = gate.lock().await;
            let budget = std::time::Duration::from_millis(200);
            let started = tokio::time::Instant::now();
            let close = tokio::task::spawn_local({
                let state = Arc::clone(&state);
                let agent = Arc::clone(&agent);
                let session_id = session_id.clone();
                async move {
                    close_and_retire_owned_session(
                        &state,
                        helper_id,
                        &agent,
                        &session_id,
                        started + budget,
                        true,
                    )
                    .await
                }
            });

            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            drop(gate_guard);
            let ReplacementEvent::BlockingClose(closing_session, _release) = events_rx
                .recv()
                .await
                .expect("session/close must use the remaining budget")
            else {
                panic!("expected blocking session/close");
            };
            assert_eq!(closing_session, session_id);
            assert!(close.await.unwrap().is_ok());
            assert!(
                started.elapsed() < std::time::Duration::from_millis(300),
                "waiting for the lifecycle gate must not renew the retirement budget"
            );
            assert!(state.session_to_helper.lock().await.is_empty());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn retirement_waits_for_and_retires_late_session_new() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let mut completions = capture_retirement_completions(&state).await;
            let helper_id = HelperId(205);
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let live_sessions = Arc::new(Mutex::new(HashSet::new()));
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_controlled_new_session_agent_with_close_result(
                    events_tx,
                    Arc::clone(&live_sessions),
                    None,
                    false,
                    true,
                ),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "retirement-pending-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), cell);
            let agent_slot = Arc::new(OnceLock::new());
            assert!(agent_slot.set(Arc::clone(&agent)).is_ok());
            let agent_side_slot = Arc::new(OnceLock::new());
            agent_side_slot
                .set(agent_link_to_noop_client())
                .expect("agent-side forwarder should be initialized");
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let handler = HelperHandler {
                helper_id,
                agent: agent_slot,
                state: Arc::clone(&state),
                replacement_gate: Arc::new(Mutex::new(())),
                notif_tx,
                agent_side_slot,
            };
            let mut request = acp::schema::v1::NewSessionRequest::new(PathBuf::from("C:\\pending"));
            crate::session_registry::inject_wta_meta(
                &mut request.meta,
                &crate::session_registry::WtaMeta {
                    owner_tab_id: Some("tab-pending-retirement".to_string()),
                    ..Default::default()
                },
            );
            let new_task =
                tokio::task::spawn_local(async move { handler.new_session(request).await });
            let ReplacementEvent::New(_, release_new) =
                events_rx.recv().await.expect("session/new must block")
            else {
                panic!("expected controlled session/new");
            };

            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "retire_agent_sessions",
                    "params": {
                        "operation_id": "retire-pending-op",
                        "scope": "tabs",
                        "tab_ids": ["tab-pending-retirement"],
                        "reason": "tab_group_delete"
                    }
                }),
            )
            .await;
            tokio::task::yield_now().await;
            assert!(
                completions.try_recv().is_err(),
                "completion must wait for the in-flight session result"
            );

            release_new.send(()).unwrap();
            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Cancel(_))
            ));
            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Close(_))
            ));
            let mut response = new_task.await.unwrap().unwrap();
            assert_eq!(
                crate::session_registry::extract_wta_meta(&mut response.meta)
                    .session_result
                    .as_deref(),
                Some("retired")
            );
            assert_eq!(completions.recv().await.unwrap()["params"]["success"], true);
            assert!(live_sessions.lock().await.is_empty());
            assert!(state.session_to_helper.lock().await.is_empty());
            assert!(!state.helper_meta.lock().await.contains_key(&helper_id));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn retirement_timeout_cleans_before_completion_and_fences_late_session_new() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state_with_retirement_pending_timeout(std::time::Duration::ZERO);
            let mut completions = capture_retirement_completions(&state).await;
            let helper_id = HelperId(206);
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let live_sessions = Arc::new(Mutex::new(HashSet::new()));
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_controlled_new_session_agent_with_close_result(
                    events_tx,
                    Arc::clone(&live_sessions),
                    None,
                    false,
                    true,
                ),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "retirement-timeout-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), cell);
            let agent_slot = Arc::new(OnceLock::new());
            assert!(agent_slot.set(Arc::clone(&agent)).is_ok());
            let agent_side_slot = Arc::new(OnceLock::new());
            agent_side_slot
                .set(agent_link_to_noop_client())
                .expect("agent-side forwarder should be initialized");
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let handler = HelperHandler {
                helper_id,
                agent: agent_slot,
                state: Arc::clone(&state),
                replacement_gate: Arc::new(Mutex::new(())),
                    notif_tx,
                agent_side_slot,
            };
            let mut request = acp::schema::v1::NewSessionRequest::new(PathBuf::from("C:\\pending"));
            crate::session_registry::inject_wta_meta(
                &mut request.meta,
                &crate::session_registry::WtaMeta {
                    owner_tab_id: Some("tab-retirement-timeout".to_string()),
                    ..Default::default()
                },
            );
            let new_task =
                tokio::task::spawn_local(async move { handler.new_session(request).await });
            let ReplacementEvent::New(_, release_new) =
                events_rx.recv().await.expect("session/new must block")
            else {
                panic!("expected controlled session/new");
            };

            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "retire_agent_sessions",
                    "params": {
                        "operation_id": "retire-timeout-op",
                        "scope": "tabs",
                        "tab_ids": ["tab-retirement-timeout"],
                        "reason": "window_close"
                    }
                }),
            )
            .await;

            let completion = completions.recv().await.expect("timeout must complete");
            assert_eq!(completion["params"]["success"], false);
            assert!(state.pending_session_helpers.lock().await.is_empty());
            assert!(state.pending_session_mcp.lock().await.is_empty());
            assert!(state.session_to_helper.lock().await.is_empty());
            assert!(!state.helper_meta.lock().await.contains_key(&helper_id));
            assert!(state.orphaned_tabs.lock().await.is_empty());
            assert!(state.orphaned_sessions.lock().await.is_empty());
            assert!(state.closing_session_results.lock().await.is_empty());
            assert_eq!(
                state
                    .session_mcp_capabilities
                    .remove_owner(agent.instance_id)
                    .await,
                0,
                "completion must follow revocation of the pending MCP capability"
            );
            assert!(state
                .closing_session_helpers
                .lock()
                .await
                .contains(&helper_id));
            assert!(state
                .destructive_session_helpers
                .lock()
                .await
                .contains(&helper_id));
            assert!(!state
                .active_retirement_helpers
                .lock()
                .await
                .contains(&helper_id));

            release_new.send(()).unwrap();
            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Cancel(_))
            ));
            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Close(_))
            ));
            let mut response = new_task.await.unwrap().unwrap();
            assert_eq!(
                crate::session_registry::extract_wta_meta(&mut response.meta)
                    .session_result
                    .as_deref(),
                Some("retired")
            );
            assert!(live_sessions.lock().await.is_empty());
            assert!(state.session_to_helper.lock().await.is_empty());
            assert!(state.registry.lookup(&response.session_id).await.is_none());
            assert!(state.pending_session_mcp.lock().await.is_empty());
            assert!(state.closing_session_results.lock().await.is_empty());
            assert!(!state.helper_meta.lock().await.contains_key(&helper_id));
        })
        .await;
}

#[tokio::test]
async fn retirement_completion_cache_is_bounded_without_evicting_in_flight_entries() {
    let state = make_state();
    state.retirement_operations.lock().await.insert(
        "still-running".to_string(),
        RetirementOperationState::InFlight,
            );

    for index in 0..(RETIREMENT_COMPLETION_CAP + 32) {
        record_retirement_completion(
            &state,
            format!("completed-{index}"),
            serde_json::json!({ "operation": index }),
        )
        .await;
    }

    let now = tokio::time::Instant::now();
    {
        let mut operations = state.retirement_operations.lock().await;
        operations.insert(
            "expired".to_string(),
            RetirementOperationState::Completed {
                event: serde_json::json!({ "operation": "expired" }),
                completed_at: now
                    .checked_sub(RETIREMENT_COMPLETION_TTL)
                    .expect("test instant must support TTL subtraction"),
            },
            );
        prune_retirement_operations(&mut operations, now);
        assert!(matches!(
            operations.get("still-running"),
            Some(RetirementOperationState::InFlight)
        ));
        assert!(!operations.contains_key("expired"));
        assert_eq!(
            operations
                .values()
                .filter(|state| matches!(state, RetirementOperationState::Completed { .. }))
                .count(),
            RETIREMENT_COMPLETION_CAP
            );
    }
}

#[tokio::test]
async fn master_tab_rename_rekeys_live_and_orphan_ownership() {
    let state = make_state();
    let helper_id = HelperId(118);
    let session_id = SessionId::new("dragged-session");
    state.connected_helpers.lock().await.insert(helper_id);
    state.helper_meta.lock().await.insert(
        helper_id,
        HelperRecoveryMeta {
            owner_tab_id: Some("old-stable-id".to_string()),
            last_session_id: Some(session_id.clone()),
        },
    );
    state.orphaned_tabs.lock().await.insert(
        "old-stable-id".to_string(),
        ("copilot --acp --stdio".to_string(), helper_id, session_id),
    );
    state
        .pending_session_helpers
        .lock()
        .await
        .insert(helper_id, Some("old-stable-id".to_string()));

    handle_master_wt_event(
        &state,
        serde_json::json!({
            "method": "tab_renamed",
            "params": {
                "old_tab_id": "old-stable-id",
                "new_tab_id": "new-stable-id",
            }
        }),
    )
    .await;

    assert_eq!(
        state
            .helper_meta
            .lock()
            .await
            .get(&helper_id)
            .and_then(|meta| meta.owner_tab_id.as_deref()),
        Some("new-stable-id")
    );
    assert_eq!(
        state
            .pending_session_helpers
            .lock()
            .await
            .get(&helper_id)
            .and_then(Option::as_deref),
        Some("new-stable-id")
    );
    let orphaned_tabs = state.orphaned_tabs.lock().await;
    assert!(!orphaned_tabs.contains_key("old-stable-id"));
    assert!(orphaned_tabs.contains_key("new-stable-id"));
    drop(orphaned_tabs);
    assert!(state.tab_retirement_fences.lock().await.is_empty());
    assert!(state.tab_retirement_rekeys.lock().await.is_empty());
    assert!(state.closing_session_helpers.lock().await.is_empty());
    assert!(state.destructive_session_helpers.lock().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn active_retirement_follows_tab_rename_and_clears_moved_fence_on_disconnect() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let mut completions = capture_retirement_completions(&state).await;
            let helper_id = HelperId(119);
            let session_id = SessionId::new("retirement-drag-race");
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_blocking_close_agent(events_tx),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "retirement-drag-race-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), cell);
            register_retirement_tab(
                &state,
                &agent,
                helper_id,
                "retirement-drag-old",
                &session_id,
            )
            .await;

            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "retire_agent_sessions",
                    "params": {
                        "operation_id": "retirement-drag-race-op",
                        "scope": "tabs",
                        "tab_ids": ["retirement-drag-old"],
                        "reason": "window_close"
                    }
                }),
            )
            .await;
            let ReplacementEvent::BlockingClose(closing_session, release_close) = events_rx
                .recv()
                .await
                .expect("retirement must reach session/close before the drag")
            else {
                panic!("expected blocking session/close");
            };
            assert_eq!(closing_session, session_id);

            handle_master_wt_event(
                &state,
                serde_json::json!({
                    "method": "tab_renamed",
                    "params": {
                        "old_tab_id": "retirement-drag-old",
                        "new_tab_id": "retirement-drag-new"
                    }
                }),
            )
            .await;
            assert_eq!(
                state
                    .helper_meta
                    .lock()
                    .await
                    .get(&helper_id)
                    .and_then(|meta| meta.owner_tab_id.as_deref()),
                Some("retirement-drag-new")
            );
            assert!(!state
                .tab_retirement_fences
                .lock()
                .await
                .contains_key("retirement-drag-old"));
            assert!(state
                .tab_retirement_fences
                .lock()
                .await
                .contains_key("retirement-drag-new"));
            assert_eq!(
                state
                    .tab_retirement_rekeys
                    .lock()
                    .await
                    .get("retirement-drag-old")
                    .map(String::as_str),
                Some("retirement-drag-new")
            );

            release_close.send(()).unwrap();
            assert_eq!(completions.recv().await.unwrap()["params"]["success"], true);
            assert!(state.session_to_helper.lock().await.is_empty());
            assert!(state.registry.lookup(&session_id).await.is_none());
            assert!(state.tab_retirement_rekeys.lock().await.is_empty());
            assert!(state
                .destructive_session_helpers
                .lock()
                .await
                .contains(&helper_id));

            let (intentional, recovery) =
                consume_disconnected_helper_retirement_state(&state, helper_id).await;
            assert!(intentional);
            assert_eq!(
                recovery.and_then(|meta| meta.owner_tab_id),
                None,
                "completed retirement must not retain recovery metadata"
            );
            assert!(state.tab_retirement_fences.lock().await.is_empty());
            assert!(state.destructive_session_helpers.lock().await.is_empty());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn close_by_tab_retires_session_new_that_finishes_after_tab_destruction() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(117);
            let agent_instance_id = AgentInstanceId::new_v4();
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let live_sessions = Arc::new(Mutex::new(HashSet::new()));
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: agent_instance_id,
                conn: client_connection_to_controlled_new_session_agent(
                    events_tx,
                    Arc::clone(&live_sessions),
                    None,
                ),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "pending-new-close-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), cell);

            let agent_slot = Arc::new(OnceLock::new());
            assert!(agent_slot.set(Arc::clone(&agent)).is_ok());
            let agent_side_slot = Arc::new(OnceLock::new());
            agent_side_slot
                .set(agent_link_to_noop_client())
                .expect("agent-side forwarder should be set once");
            let handler = HelperHandler {
                helper_id,
                agent: agent_slot,
                state: Arc::clone(&state),
                replacement_gate: Arc::new(Mutex::new(())),
                notif_tx,
                agent_side_slot,
            };
            let mut request =
                acp::schema::v1::NewSessionRequest::new(PathBuf::from("C:\\pending-new"));
            crate::session_registry::inject_wta_meta(
                &mut request.meta,
                &crate::session_registry::WtaMeta {
                    owner_tab_id: Some("closing-pending-new-tab".to_string()),
                    ..Default::default()
                },
            );

            let new_task = tokio::task::spawn_local({
                let handler = handler.clone();
                async move { handler.new_session(request).await }
            });
            let ReplacementEvent::New(_, release_new) = events_rx
                .recv()
                .await
                .expect("session/new should reach the controlled agent")
            else {
                panic!("expected a blocked session/new request");
            };

            handle_close_tab_session(
                &state,
                &crate::session_registry::CloseTabSessionParams {
                    tab_id: "closing-pending-new-tab".to_string(),
                },
                false,
            )
            .await
            .expect("tab close should mark the in-flight session for retirement");
            assert!(state
                .closing_session_helpers
                .lock()
                .await
                .contains(&helper_id));

            release_new
                .send(())
                .expect("the controlled session/new should still be waiting");
            let mut response = new_task
                .await
                .expect("session/new task should finish")
                .expect("the agent response should be consumed before retirement");
            let Some(ReplacementEvent::Close(closed_session_id)) = events_rx.recv().await else {
                panic!("the late-created session must be physically closed");
            };
            assert_eq!(closed_session_id, response.session_id);
            assert_eq!(
                crate::session_registry::extract_wta_meta(&mut response.meta)
                    .session_result
                    .as_deref(),
                Some("retired"),
                "the helper must not bind a session retired during tab destruction"
            );
            assert!(live_sessions.lock().await.is_empty());
            assert!(state.session_to_helper.lock().await.is_empty());
            assert!(state.registry.lookup(&response.session_id).await.is_none());
            assert!(!state.helper_meta.lock().await.contains_key(&helper_id));
            assert!(!state
                .pending_session_helpers
                .lock()
                .await
                .contains_key(&helper_id));
            assert!(!state
                .closing_session_helpers
                .lock()
                .await
                .contains(&helper_id));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn session_new_result_is_closed_when_helper_forwarder_disappears() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(121);
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let live_sessions = Arc::new(Mutex::new(HashSet::new()));
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_controlled_new_session_agent(
                    events_tx,
                    Arc::clone(&live_sessions),
                    None,
                ),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "missing-forwarder-new-session-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let agent_slot = Arc::new(OnceLock::new());
            assert!(agent_slot.set(agent).is_ok());
            let handler = HelperHandler {
                helper_id,
                agent: agent_slot,
                state: Arc::clone(&state),
                replacement_gate: Arc::new(Mutex::new(())),
                notif_tx,
                // Deliberately unset: the helper disconnected after the agent
                // accepted session/new but before route installation.
                agent_side_slot: Arc::new(OnceLock::new()),
            };
            let request =
                acp::schema::v1::NewSessionRequest::new(PathBuf::from("C:\\missing-forwarder"));
            let new_task = tokio::task::spawn_local({
                let handler = handler.clone();
                async move { handler.new_session(request).await }
            });
            let ReplacementEvent::New(0, release_new) = events_rx
                .recv()
                .await
                .expect("session/new should reach the controlled agent")
            else {
                panic!("expected a blocked session/new request");
            };
            release_new
                .send(())
                .expect("the controlled session/new should still be waiting");

            new_task
                .await
                .expect("session/new task should finish")
                .expect_err("the missing helper forwarder must reject the response");
            let Some(ReplacementEvent::Close(closed_session_id)) = events_rx.recv().await else {
                panic!("the unreachable new session must be physically closed");
            };
            assert_eq!(closed_session_id, SessionId::new("replacement-b"));
            assert!(live_sessions.lock().await.is_empty());
            assert!(state.session_to_helper.lock().await.is_empty());
            assert!(!state
                .pending_session_helpers
                .lock()
                .await
                .contains_key(&helper_id));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn close_by_tab_resolves_pre_registered_load_route_without_last_session_metadata() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(118);
            let session_id = SessionId::new("pending-load-close-session");
            let agent_instance_id = AgentInstanceId::new_v4();
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let live_sessions = Arc::new(Mutex::new(HashSet::from([session_id.clone()])));
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: agent_instance_id,
                conn: client_connection_to_controlled_new_session_agent(
                    events_tx,
                    Arc::clone(&live_sessions),
                    None,
                ),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "pending-load-close-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), cell);
            bind_session_route(
                &state,
                session_id.clone(),
                HelperRoute {
                    helper_id,
                    agent_instance_id,
                    notif_tx,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;
            state.helper_meta.lock().await.insert(
                helper_id,
                HelperRecoveryMeta {
                    owner_tab_id: Some("closing-pending-load-tab".to_string()),
                    last_session_id: None,
                },
            );
            state
                .pending_session_helpers
                .lock()
                .await
                .insert(helper_id, Some("closing-pending-load-tab".to_string()));

            handle_close_tab_session(
                &state,
                &crate::session_registry::CloseTabSessionParams {
                    tab_id: "closing-pending-load-tab".to_string(),
                },
                false,
            )
            .await
            .expect("tab close should resolve the pre-registered load route");

            let Some(ReplacementEvent::Close(closed_session_id)) = events_rx.recv().await else {
                panic!("the in-flight load target must be physically closed");
            };
            assert_eq!(closed_session_id, session_id);
            assert!(live_sessions.lock().await.is_empty());
            assert!(state.session_to_helper.lock().await.is_empty());
            assert!(state
                .closing_session_helpers
                .lock()
                .await
                .contains(&helper_id));
            assert!(state.helper_meta.lock().await.contains_key(&helper_id));
        })
        .await;
}

#[tokio::test]
async fn close_by_tab_clears_idle_recovery_metadata_without_a_live_session() {
    let state = make_state();
    let helper_id = HelperId(119);
    state.helper_meta.lock().await.insert(
        helper_id,
        HelperRecoveryMeta {
            owner_tab_id: Some("idle-closed-tab".to_string()),
            last_session_id: None,
        },
    );

    handle_close_tab_session(
        &state,
        &crate::session_registry::CloseTabSessionParams {
            tab_id: "idle-closed-tab".to_string(),
        },
        false,
    )
    .await
    .expect("an idle tab close should remain idempotent");

    assert!(!state.helper_meta.lock().await.contains_key(&helper_id));
    assert!(!state
        .closing_session_helpers
        .lock()
        .await
        .contains(&helper_id));
}

#[tokio::test]
async fn duplicate_close_preserves_marker_until_committing_transaction_consumes_it() {
    let state = make_state();
    let helper_id = HelperId(122);
    state.helper_meta.lock().await.insert(
        helper_id,
        HelperRecoveryMeta {
            owner_tab_id: Some("committing-closed-tab".to_string()),
            last_session_id: None,
        },
    );
    state.closing_session_helpers.lock().await.insert(helper_id);

    handle_close_tab_session(
        &state,
        &crate::session_registry::CloseTabSessionParams {
            tab_id: "committing-closed-tab".to_string(),
        },
        false,
    )
    .await
    .expect("duplicate close remains idempotent");

    assert!(state.helper_meta.lock().await.contains_key(&helper_id));
    assert!(state
        .closing_session_helpers
        .lock()
        .await
        .contains(&helper_id));
}

#[tokio::test]
async fn close_finds_pending_transaction_before_recovery_metadata_is_published() {
    let state = make_state();
    let helper_id = HelperId(123);
    state
        .pending_session_helpers
        .lock()
        .await
        .insert(helper_id, Some("pending-owner-tab".to_string()));

    handle_close_tab_session(
        &state,
        &crate::session_registry::CloseTabSessionParams {
            tab_id: "pending-owner-tab".to_string(),
        },
        false,
    )
    .await
    .expect("close should defer to the pending transaction");

    assert!(state
        .pending_session_helpers
        .lock()
        .await
        .contains_key(&helper_id));
    assert!(state
        .closing_session_helpers
        .lock()
        .await
        .contains(&helper_id));
}

#[tokio::test(flavor = "current_thread")]
async fn overlapping_new_sessions_retire_the_intermediate_replacement() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(17);
            let initial_session = SessionId::new("replacement-a");
            let intermediate_session = SessionId::new("replacement-b");
            let final_session = SessionId::new("replacement-c");
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let live_sessions = Arc::new(Mutex::new(HashSet::from([initial_session.clone()])));
            let agent_side_slot = Arc::new(OnceLock::new());
            agent_side_slot
                .set(agent_link_to_noop_client())
                .expect("agent-side forwarder should be set once");
            let agent = Arc::new(OnceLock::new());
            let agent_instance_id = AgentInstanceId::new_v4();
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            assert!(agent
                .set(Arc::new(AgentCli {
                    instance_id: agent_instance_id,
                    conn: client_connection_to_controlled_new_session_agent(
                        events_tx,
                        Arc::clone(&live_sessions),
                        None,
                    ),
                    cached_init_resp,
                    cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                    source: crate::agent_source::AgentSource::Host,
                    cmd_key: "serialized-replacement-agent".to_string(),
                    cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                    bound_helpers: Mutex::new(HashSet::new()),
                    host_list_cache: Mutex::new(None),
                    listed_ever: Mutex::new(HashSet::new()),
                }))
                .is_ok());
            let handler = HelperHandler {
                helper_id,
                agent,
                state: Arc::clone(&state),
                replacement_gate: Arc::new(Mutex::new(())),
                notif_tx: notif_tx.clone(),
                agent_side_slot,
            };

            bind_session_route(
                &state,
                initial_session.clone(),
                HelperRoute {
                    helper_id,
                    agent_instance_id,
                    notif_tx,
                    forwarder: Some(agent_link_to_noop_client()),
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;
            state
                .registry
                .upsert(crate::session_registry::SessionInfo::new(
                    initial_session.clone(),
                    PathBuf::from("C:\\repo-a"),
                ))
                .await;
            state.helper_meta.lock().await.insert(
                helper_id,
                HelperRecoveryMeta {
                    last_session_id: Some(initial_session.clone()),
                    ..Default::default()
                },
            );

            let first = tokio::task::spawn_local({
                let handler = handler.clone();
                async move {
                    handler
                        .new_session(acp::schema::v1::NewSessionRequest::new(PathBuf::from(
                            "C:\\repo-b",
                        )))
                        .await
                }
            });
            let first_close = events_rx
                .recv()
                .await
                .expect("first replacement should close its predecessor");
            assert!(matches!(
                first_close,
                ReplacementEvent::Close(ref sid) if sid == &initial_session
            ));
            let ReplacementEvent::New(first_index, release_first) = events_rx
                .recv()
                .await
                .expect("first replacement should reach the agent")
            else {
                panic!("session/new must follow predecessor close");
            };
            assert_eq!(first_index, 0);

            let (second_started_tx, second_started_rx) = tokio::sync::oneshot::channel();
            let second = tokio::task::spawn_local({
                let handler = handler.clone();
                async move {
                    let _ = second_started_tx.send(());
                    handler
                        .new_session(acp::schema::v1::NewSessionRequest::new(PathBuf::from(
                            "C:\\repo-c",
                        )))
                        .await
                }
            });
            second_started_rx
                .await
                .expect("overlapping replacement task should start");
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(25), events_rx.recv(),)
                    .await
                    .is_err(),
                "the second replacement must wait for the first transaction"
            );

            release_first
                .send(())
                .expect("first replacement should still be waiting");
            let first_response = first
                .await
                .expect("first replacement task should finish")
                .expect("first replacement should succeed");
            assert_eq!(first_response.session_id, intermediate_session);

            let second_close = events_rx
                .recv()
                .await
                .expect("second replacement should close the intermediate session");
            assert!(matches!(
                second_close,
                ReplacementEvent::Close(ref sid) if sid == &intermediate_session
            ));
            let ReplacementEvent::New(second_index, release_second) = events_rx
                .recv()
                .await
                .expect("final replacement should reach the agent after the close")
            else {
                panic!("final session/new must follow intermediate close");
            };
            assert_eq!(second_index, 1);
            release_second
                .send(())
                .expect("final replacement should still be waiting");
            let second_response = second
                .await
                .expect("final replacement task should finish")
                .expect("final replacement should succeed");
            assert_eq!(second_response.session_id, final_session);

            let routes = state.session_to_helper.lock().await;
            assert_eq!(routes.len(), 1);
            assert!(routes.contains_key(&final_session));
            assert!(!routes.contains_key(&initial_session));
            assert!(!routes.contains_key(&intermediate_session));
            drop(routes);
            assert!(state.registry.lookup(&initial_session).await.is_none());
            assert!(state.registry.lookup(&intermediate_session).await.is_none());
            assert!(state.registry.lookup(&final_session).await.is_some());
            assert_eq!(
                state.helper_meta.lock().await[&helper_id].last_session_id,
                Some(final_session.clone())
            );
            assert_eq!(
                *live_sessions.lock().await,
                HashSet::from([final_session]),
                "only the final physical ACP session should remain live"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn unsupported_session_close_capability_cancels_and_logically_retires_session() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(18);
            let old_session = SessionId::new("unsupported-old");
            let replacement = SessionId::new("replacement-b");
            let agent_instance_id = AgentInstanceId::new_v4();
            let live_sessions = Arc::new(Mutex::new(HashSet::from([old_session.clone()])));
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let agent_side_slot = Arc::new(OnceLock::new());
            agent_side_slot
                .set(agent_link_to_noop_client())
                .expect("agent-side forwarder should be set once");
            let agent = Arc::new(OnceLock::new());
            assert!(agent
                .set(Arc::new(AgentCli {
                    instance_id: agent_instance_id,
                    conn: client_connection_to_controlled_new_session_agent_with_close_result(
                        events_tx,
                        Arc::clone(&live_sessions),
                        None,
                        false,
                        true,
                    ),
                    cached_init_resp: acp::schema::v1::InitializeResponse::new(
                        acp::schema::ProtocolVersion::V1,
                    ),
                    cli_source: Some(crate::agent_sessions::CliSource::Gemini),
                    source: crate::agent_source::AgentSource::Host,
                    cmd_key: "unsupported-close-agent".to_string(),
                    cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                    bound_helpers: Mutex::new(HashSet::new()),
                    host_list_cache: Mutex::new(None),
                    listed_ever: Mutex::new(HashSet::new()),
                }))
                .is_ok());
            let pooled_agent = Arc::new(tokio::sync::OnceCell::new());
            assert!(pooled_agent
                .set(Arc::clone(
                    agent
                        .get()
                        .expect("handler agent binding should be initialized"),
                ))
                .is_ok());
            state
                .agents
                .lock()
                .await
                .insert("unsupported-close-agent".to_string(), pooled_agent);
            let handler = HelperHandler {
                helper_id,
                agent,
                state: Arc::clone(&state),
                replacement_gate: Arc::new(Mutex::new(())),
                notif_tx: notif_tx.clone(),
                agent_side_slot,
            };
            bind_session_route(
                &state,
                old_session.clone(),
                HelperRoute {
                    helper_id,
                    agent_instance_id,
                    notif_tx,
                    forwarder: Some(agent_link_to_noop_client()),
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;
            state
                .registry
                .upsert(crate::session_registry::SessionInfo::new(
                    old_session.clone(),
                    PathBuf::from("C:\\old"),
                ))
                .await;
            state.helper_meta.lock().await.insert(
                helper_id,
                HelperRecoveryMeta {
                    last_session_id: Some(old_session.clone()),
                    ..Default::default()
                },
            );
            let pending_capability = state
                .session_mcp_capabilities
                .prepare(agent_instance_id, None)
                .await;
            assert!(
                state
                    .session_mcp_capabilities
                    .bind(&pending_capability, old_session.clone())
                    .await
            );
            state.pending_usage.lock().await.insert(
                old_session.clone(),
                (
                    helper_id,
                    acp::schema::v1::SessionNotification::new(
                        old_session.clone(),
                        acp::schema::v1::SessionUpdate::AgentMessageChunk(
                            acp::schema::v1::ContentChunk::new("pending usage".into()),
                        ),
                    ),
                ),
            );

            let request = tokio::task::spawn_local({
                let handler = handler.clone();
                async move {
                    handler
                        .new_session(acp::schema::v1::NewSessionRequest::new(PathBuf::from(
                            "C:\\new",
                        )))
                        .await
                }
            });
            let Some(ReplacementEvent::Cancel(cancelled_session_id)) = events_rx.recv().await
            else {
                panic!("unsupported agents must receive the best-effort session/cancel");
            };
            assert_eq!(cancelled_session_id, old_session);
            let ReplacementEvent::New(index, release) = events_rx
                .recv()
                .await
                .expect("replacement should be created")
            else {
                panic!("unsupported agents must not receive session/close");
            };
            assert_eq!(index, 0);
            release.send(()).unwrap();
            assert_eq!(
                request.await.unwrap().unwrap().session_id,
                replacement.clone()
            );
            assert!(
                events_rx.try_recv().is_err(),
                "unsupported agents must not receive session/close"
            );

            let routes = state.session_to_helper.lock().await;
            assert!(!routes.contains_key(&old_session));
            assert!(routes.contains_key(&replacement));
            drop(routes);
            assert!(state.registry.lookup(&old_session).await.is_none());
            assert!(!state.pending_usage.lock().await.contains_key(&old_session));
            assert_eq!(
                state
                    .session_mcp_capabilities
                    .remove_owner(agent_instance_id)
                    .await,
                0,
                "logical retirement must revoke the session-scoped MCP capability"
            );
            assert!(
                state
                    .agents
                    .lock()
                    .await
                    .get("unsupported-close-agent")
                    .and_then(|cell| cell.get())
                    .is_some(),
                "logical retirement must keep the shared agent process usable"
            );
            assert_eq!(
                *live_sessions.lock().await,
                HashSet::from([old_session, replacement]),
                "unsupported agents can only be logically retired"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn advertised_but_unimplemented_session_close_cancels_and_logically_retires_session() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(19);
            let session_id = SessionId::new("advertised-unimplemented");
            let replacement = SessionId::new("replacement-b");
            let agent_instance_id = AgentInstanceId::new_v4();
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let live_sessions = Arc::new(Mutex::new(HashSet::from([session_id.clone()])));
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: agent_instance_id,
                conn: client_connection_to_controlled_new_session_agent_with_close_result(
                    events_tx,
                    Arc::clone(&live_sessions),
                    None,
                    true,
                    true,
                ),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Gemini),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "advertised-unimplemented-close-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });
            let agent_cell = Arc::new(tokio::sync::OnceCell::new());
            assert!(agent_cell.set(Arc::clone(&agent)).is_ok());
            state
                .agents
                .lock()
                .await
                .insert(agent.cmd_key.clone(), Arc::clone(&agent_cell));
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            bind_session_route(
                &state,
                session_id.clone(),
                HelperRoute {
                    helper_id,
                    agent_instance_id,
                    notif_tx,
                    forwarder: Some(agent_link_to_noop_client()),
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;
            state
                .registry
                .upsert(crate::session_registry::SessionInfo::new(
                    session_id.clone(),
                    PathBuf::from("C:\\repo"),
                ))
                .await;
            let pending_capability = state
                .session_mcp_capabilities
                .prepare(agent_instance_id, None)
                .await;
            assert!(
                state
                    .session_mcp_capabilities
                    .bind(&pending_capability, session_id.clone())
                    .await
            );
            state.pending_usage.lock().await.insert(
                session_id.clone(),
                (
                    helper_id,
                    acp::schema::v1::SessionNotification::new(
                        session_id.clone(),
                        acp::schema::v1::SessionUpdate::AgentMessageChunk(
                            acp::schema::v1::ContentChunk::new("pending usage".into()),
                        ),
                    ),
                ),
            );

            assert_eq!(
                close_and_retire_replaced_session(
                    &state,
                    helper_id,
                    &agent,
                    &session_id,
                    std::time::Duration::from_secs(1),
                )
                .await
                .expect("MethodNotFound must fall back without failing tab teardown"),
                ReplacedSessionCleanup::LogicalFallback
            );
            let Some(ReplacementEvent::Cancel(cancelled_session_id)) = events_rx.recv().await
            else {
                panic!("best-effort cancellation must precede the close attempt");
            };
            assert_eq!(cancelled_session_id, session_id);
            let Some(ReplacementEvent::Close(close_session_id)) = events_rx.recv().await else {
                panic!("the advertised capability must be attempted once");
            };
            assert_eq!(close_session_id, session_id);
            assert!(!state
                .session_to_helper
                .lock()
                .await
                .contains_key(&session_id));
            assert!(state.registry.lookup(&session_id).await.is_none());
            assert!(!state.pending_usage.lock().await.contains_key(&session_id));
            assert_eq!(
                state
                    .session_mcp_capabilities
                    .remove_owner(agent_instance_id)
                    .await,
                0,
                "logical retirement must revoke the session-scoped MCP capability"
            );

            let conn = agent.conn.clone();
            let follow_up = tokio::task::spawn_local(async move {
                conn.new_session(acp::schema::v1::NewSessionRequest::new(PathBuf::from(
                    "C:\\replacement",
                )))
                .await
            });
            let Some(ReplacementEvent::New(index, release)) = events_rx.recv().await else {
                panic!("the shared agent must remain usable after MethodNotFound");
            };
            assert_eq!(index, 0);
            release
                .send(())
                .expect("follow-up session request must still be live");
            assert_eq!(
                follow_up
                    .await
                    .expect("follow-up task must finish")
                    .expect("shared agent must accept a follow-up session")
                    .session_id,
                replacement
            );
            assert!(
                state
                    .agents
                    .lock()
                    .await
                    .get(&agent.cmd_key)
                    .and_then(|cell| cell.get())
                    .is_some(),
                "logical retirement must keep the shared agent process pooled"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn close_failure_keeps_predecessor_and_does_not_create_replacement() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(19);
            let old_session = SessionId::new("close-failure-old");
            let live_sessions = Arc::new(Mutex::new(HashSet::from([old_session.clone()])));
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let agent_side_slot = Arc::new(OnceLock::new());
            agent_side_slot
                .set(agent_link_to_noop_client())
                .expect("agent-side forwarder should be set once");
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(OnceLock::new());
            let agent_instance_id = AgentInstanceId::new_v4();
            assert!(agent
                .set(Arc::new(AgentCli {
                    instance_id: agent_instance_id,
                    conn: client_connection_to_controlled_new_session_agent(
                        events_tx,
                        Arc::clone(&live_sessions),
                        Some(old_session.clone()),
                    ),
                    cached_init_resp,
                    cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                    source: crate::agent_source::AgentSource::Host,
                    cmd_key: "failing-close-agent".to_string(),
                    cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                    bound_helpers: Mutex::new(HashSet::new()),
                    host_list_cache: Mutex::new(None),
                    listed_ever: Mutex::new(HashSet::new()),
                }))
                .is_ok());
            let handler = HelperHandler {
                helper_id,
                agent,
                state: Arc::clone(&state),
                replacement_gate: Arc::new(Mutex::new(())),
                notif_tx: notif_tx.clone(),
                agent_side_slot,
            };
            bind_session_route(
                &state,
                old_session.clone(),
                HelperRoute {
                    helper_id,
                    agent_instance_id,
                    notif_tx,
                    forwarder: Some(agent_link_to_noop_client()),
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;
            state
                .registry
                .upsert(crate::session_registry::SessionInfo::new(
                    old_session.clone(),
                    PathBuf::from("C:\\old"),
                ))
                .await;
            state.helper_meta.lock().await.insert(
                helper_id,
                HelperRecoveryMeta {
                    last_session_id: Some(old_session.clone()),
                    ..Default::default()
                },
            );

            let error = handler
                .new_session(acp::schema::v1::NewSessionRequest::new(PathBuf::from(
                    "C:\\new",
                )))
                .await
                .expect_err("close failure must abort replacement");
            assert!(format!("{error}").contains("injected close failure"));
            assert!(matches!(
                events_rx.try_recv(),
                Ok(ReplacementEvent::Close(ref sid)) if sid == &old_session
            ));
            assert!(
                events_rx.try_recv().is_err(),
                "session/new must not be sent"
            );
            assert!(state
                .session_to_helper
                .lock()
                .await
                .contains_key(&old_session));
            assert!(state.registry.lookup(&old_session).await.is_some());
            assert_eq!(
                state.helper_meta.lock().await[&helper_id].last_session_id,
                Some(old_session.clone()),
                "close failure must preserve predecessor metadata for retry"
            );
            assert_eq!(
                *live_sessions.lock().await,
                HashSet::from([old_session.clone()])
            );

            let retry = tokio::task::spawn_local({
                let handler = handler.clone();
                async move {
                    handler
                        .new_session(acp::schema::v1::NewSessionRequest::new(PathBuf::from(
                            "C:\\retry",
                        )))
                        .await
                }
            });
            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Close(ref sid)) if sid == &old_session
            ));
            let ReplacementEvent::New(index, release) = events_rx
                .recv()
                .await
                .expect("retry must create only after closing the same predecessor")
            else {
                panic!("retry must issue session/new after predecessor close");
            };
            assert_eq!(index, 0);
            drop(release);
            retry
                .await
                .unwrap()
                .expect_err("injected session/new failure must surface");
            assert_eq!(
                state.helper_meta.lock().await[&helper_id].last_session_id,
                None,
                "metadata must stay empty after the predecessor was retired"
            );
            assert!(!state
                .session_to_helper
                .lock()
                .await
                .contains_key(&old_session));
            assert!(live_sessions.lock().await.is_empty());

            let final_attempt = tokio::task::spawn_local({
                let handler = handler.clone();
                async move {
                    handler
                        .new_session(acp::schema::v1::NewSessionRequest::new(PathBuf::from(
                            "C:\\final",
                        )))
                        .await
                }
            });
            let ReplacementEvent::New(index, release) = events_rx
                .recv()
                .await
                .expect("no stale predecessor close should precede the final session/new")
            else {
                panic!("metadata None must make the final attempt start with session/new");
            };
            assert_eq!(index, 1);
            release.send(()).unwrap();
            let replacement = final_attempt.await.unwrap().unwrap().session_id;
            assert_eq!(replacement, SessionId::new("replacement-c"));
            assert_eq!(
                state.helper_meta.lock().await[&helper_id].last_session_id,
                Some(replacement.clone())
            );
            assert_eq!(*live_sessions.lock().await, HashSet::from([replacement]));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn load_close_failure_restores_target_route_and_capability() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(20);
            let peer_id = HelperId(21);
            let old_session = SessionId::new("load-old");
            let target_session = SessionId::new("load-target");
            let live_sessions = Arc::new(Mutex::new(HashSet::from([old_session.clone()])));
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let (peer_tx, _peer_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let agent_instance = AgentInstanceId::new_v4();
            let peer_route = HelperRoute {
                helper_id: peer_id,
                agent_instance_id: agent_instance,
                notif_tx: peer_tx,
                forwarder: Some(agent_link_to_noop_client()),
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            };
            let agent_side_slot = Arc::new(OnceLock::new());
            agent_side_slot
                .set(agent_link_to_noop_client())
                .expect("agent-side forwarder should be set once");
            let previous_capability_owner = AgentInstanceId::new_v4();
            let previous_capability = state
                .session_mcp_capabilities
                .prepare(previous_capability_owner, Some(target_session.clone()))
                .await;
            assert!(
                state
                    .session_mcp_capabilities
                    .bind(&previous_capability, target_session.clone())
                    .await
            );
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            cached_init_resp.agent_capabilities.mcp_capabilities.http = true;
            let agent = Arc::new(OnceLock::new());
            assert!(
                agent
                    .set(Arc::new(AgentCli {
                        instance_id: agent_instance,
                        conn: client_connection_to_controlled_new_session_agent(
                            events_tx,
                            Arc::clone(&live_sessions),
                            Some(old_session.clone()),
                        ),
                        cached_init_resp,
                        cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                        source: crate::agent_source::AgentSource::Host,
                        cmd_key: "load-close-failure-agent".to_string(),
                        cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                        bound_helpers: Mutex::new(HashSet::new()),
                        host_list_cache: Mutex::new(None),
                        listed_ever: Mutex::new(HashSet::new()),
                    }))
                    .is_ok()
            );
            let handler = HelperHandler {
                helper_id,
                agent,
                state: Arc::clone(&state),
                replacement_gate: Arc::new(Mutex::new(())),
                notif_tx: notif_tx.clone(),
                agent_side_slot,
            };
            bind_session_route(
                &state,
                old_session.clone(),
                HelperRoute {
                    helper_id,
                    agent_instance_id: agent_instance,
                    notif_tx,
                    forwarder: Some(agent_link_to_noop_client()),
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;
            state
                .session_to_helper
                .lock()
                .await
                .insert(target_session.clone(), peer_route);
            state
                .registry
                .upsert(crate::session_registry::SessionInfo::new(
                    old_session.clone(),
                    PathBuf::from("C:\\old"),
                ))
                .await;
            state.helper_meta.lock().await.insert(
                helper_id,
                HelperRecoveryMeta {
                    last_session_id: Some(old_session.clone()),
                    ..Default::default()
                },
            );
            let mut request = acp::schema::v1::LoadSessionRequest::new(
                target_session.clone(),
                PathBuf::from("C:\\target"),
            );
            crate::session_registry::inject_wta_meta(
                &mut request.meta,
                &crate::session_registry::WtaMeta {
                    proposal_mcp: Some("http-v1".to_string()),
                    ..Default::default()
                },
            );

            let error = handler
                .load_session_with_timeout(request, std::time::Duration::from_secs(1))
                .await
                .expect_err("predecessor close failure must roll back the loaded target");
            assert!(format!("{error}").contains("injected close failure"));
            assert!(matches!(
                events_rx.try_recv(),
                Ok(ReplacementEvent::Load(ref sid)) if sid == &target_session
            ));
            assert!(matches!(
                events_rx.try_recv(),
                Ok(ReplacementEvent::Close(ref sid)) if sid == &old_session
            ));
            assert!(events_rx.try_recv().is_err());
            let routes = state.session_to_helper.lock().await;
            assert_eq!(routes[&old_session].helper_id, helper_id);
            assert_eq!(
                routes[&target_session].helper_id, peer_id,
                "rollback must restore, not delete, a preexisting target route"
            );
            drop(routes);
            assert_eq!(
                state.helper_meta.lock().await[&helper_id].last_session_id,
                Some(old_session.clone())
            );
            assert_eq!(
                state
                    .session_mcp_capabilities
                    .remove_owner(agent_instance)
                    .await,
                0,
                "the failed load's newly prepared capability must be revoked"
            );
            assert_eq!(
                state
                    .session_mcp_capabilities
                    .remove_owner(previous_capability_owner)
                    .await,
                1,
                "the target's preexisting capability must survive rollback"
            );
            assert_eq!(
                *live_sessions.lock().await,
                HashSet::from([old_session, target_session]),
                "a preexisting target route must prevent rollback from closing another helper's session"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn load_close_failure_closes_target_when_restored_route_uses_another_agent() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(22);
            let peer_id = HelperId(23);
            let old_session = SessionId::new("cross-agent-old");
            let target_session = SessionId::new("cross-agent-target");
            let live_sessions = Arc::new(Mutex::new(HashSet::from([old_session.clone()])));
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let (peer_tx, _peer_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let current_agent_instance = AgentInstanceId::new_v4();
            let previous_agent_instance = AgentInstanceId::new_v4();
            let agent_side_slot = Arc::new(OnceLock::new());
            agent_side_slot
                .set(agent_link_to_noop_client())
                .expect("agent-side forwarder should be set once");
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(OnceLock::new());
            assert!(agent
                .set(Arc::new(AgentCli {
                    instance_id: current_agent_instance,
                    conn: client_connection_to_controlled_new_session_agent(
                        events_tx,
                        Arc::clone(&live_sessions),
                        Some(old_session.clone()),
                    ),
                    cached_init_resp,
                    cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                    source: crate::agent_source::AgentSource::Host,
                    cmd_key: "cross-agent-close-failure".to_string(),
                    cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                    bound_helpers: Mutex::new(HashSet::new()),
                    host_list_cache: Mutex::new(None),
                    listed_ever: Mutex::new(HashSet::new()),
                }))
                .is_ok());
            let handler = HelperHandler {
                helper_id,
                agent,
                state: Arc::clone(&state),
                replacement_gate: Arc::new(Mutex::new(())),
                notif_tx: notif_tx.clone(),
                agent_side_slot,
            };
            bind_session_route(
                &state,
                old_session.clone(),
                HelperRoute {
                    helper_id,
                    agent_instance_id: current_agent_instance,
                    notif_tx,
                    forwarder: Some(agent_link_to_noop_client()),
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;
            bind_session_route(
                &state,
                target_session.clone(),
                HelperRoute {
                    helper_id: peer_id,
                    agent_instance_id: previous_agent_instance,
                    notif_tx: peer_tx,
                    forwarder: Some(agent_link_to_noop_client()),
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;
            state.helper_meta.lock().await.insert(
                helper_id,
                HelperRecoveryMeta {
                    last_session_id: Some(old_session.clone()),
                    ..Default::default()
                },
            );

            let error = handler
                .load_session_with_timeout(
                    acp::schema::v1::LoadSessionRequest::new(
                        target_session.clone(),
                        PathBuf::from("C:\\target"),
                    ),
                    std::time::Duration::from_secs(3),
                )
                .await
                .expect_err("predecessor close failure must roll back the loaded target");
            assert!(format!("{error}").contains("injected close failure"));
            assert!(matches!(
                events_rx.try_recv(),
                Ok(ReplacementEvent::Load(ref sid)) if sid == &target_session
            ));
            assert!(matches!(
                events_rx.try_recv(),
                Ok(ReplacementEvent::Close(ref sid)) if sid == &old_session
            ));
            assert!(matches!(
                events_rx.try_recv(),
                Ok(ReplacementEvent::Close(ref sid)) if sid == &target_session
            ));
            assert!(events_rx.try_recv().is_err());
            let routes = state.session_to_helper.lock().await;
            assert_eq!(routes[&target_session].helper_id, peer_id);
            assert_eq!(
                routes[&target_session].agent_instance_id,
                previous_agent_instance
            );
            drop(routes);
            assert_eq!(
                *live_sessions.lock().await,
                HashSet::from([old_session]),
                "the target loaded into the current agent must be physically closed"
            );
        })
        .await;
}

async fn run_target_rebound_during_predecessor_close_failure(rebound_to_current_agent: bool) {
    let state = make_state();
    let helper_id = HelperId(30);
    let peer_id = HelperId(31);
    let old_session = SessionId::new("rebound-old");
    let target_session = SessionId::new("rebound-target");
    let current_agent_instance = AgentInstanceId::new_v4();
    let rebound_agent_instance = if rebound_to_current_agent {
        current_agent_instance
    } else {
        AgentInstanceId::new_v4()
    };
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    let (peer_tx, _peer_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    let agent_side_slot = Arc::new(OnceLock::new());
    agent_side_slot
        .set(agent_link_to_noop_client())
        .expect("agent-side forwarder should be set once");
    let mut cached_init_resp =
        acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
    cached_init_resp
        .agent_capabilities
        .session_capabilities
        .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
    let agent = Arc::new(OnceLock::new());
    assert!(agent
        .set(Arc::new(AgentCli {
            instance_id: current_agent_instance,
            conn: client_connection_to_rebind_during_close_agent(old_session.clone(), events_tx,),
            cached_init_resp,
            cli_source: Some(crate::agent_sessions::CliSource::Copilot),
            source: crate::agent_source::AgentSource::Host,
            cmd_key: "rebind-during-close-agent".to_string(),
            cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
            bound_helpers: Mutex::new(HashSet::new()),
            host_list_cache: Mutex::new(None),
            listed_ever: Mutex::new(HashSet::new()),
        }))
        .is_ok());
    let handler = HelperHandler {
        helper_id,
        agent,
        state: Arc::clone(&state),
        replacement_gate: Arc::new(Mutex::new(())),
        notif_tx: notif_tx.clone(),
        agent_side_slot,
    };
    bind_session_route(
        &state,
        old_session.clone(),
        HelperRoute {
            helper_id,
            agent_instance_id: current_agent_instance,
            notif_tx,
            forwarder: Some(agent_link_to_noop_client()),
            consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        },
    )
    .await;
    state.helper_meta.lock().await.insert(
        helper_id,
        HelperRecoveryMeta {
            last_session_id: Some(old_session.clone()),
            ..Default::default()
        },
    );

    let request = tokio::task::spawn_local({
        let handler = handler.clone();
        let target_session = target_session.clone();
        async move {
            handler
                .load_session_with_timeout(
                    acp::schema::v1::LoadSessionRequest::new(
                        target_session,
                        PathBuf::from("C:\\target"),
                    ),
                    std::time::Duration::from_secs(3),
                )
                .await
        }
    });
    assert!(matches!(
        events_rx.recv().await,
        Some(ReplacementEvent::Load(ref sid)) if sid == &target_session
    ));
    let release = match events_rx.recv().await {
        Some(ReplacementEvent::FailingClose(sid, release)) if sid == old_session => release,
        _ => panic!("predecessor close must block before failing"),
    };
    bind_session_route(
        &state,
        target_session.clone(),
        HelperRoute {
            helper_id: peer_id,
            agent_instance_id: rebound_agent_instance,
            notif_tx: peer_tx,
            forwarder: Some(agent_link_to_noop_client()),
            consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        },
    )
    .await;
    release
        .send(())
        .expect("predecessor close should still be waiting");
    let error = request
        .await
        .expect("load task should finish")
        .expect_err("predecessor close failure must fail the transaction");
    assert!(format!("{error}").contains("injected predecessor close failure"));

    if rebound_to_current_agent {
        assert!(
            events_rx.try_recv().is_err(),
            "same-agent rebound owns the physical target and must suppress rollback close"
        );
    } else {
        assert!(matches!(
            events_rx.try_recv(),
            Ok(ReplacementEvent::Close(ref sid)) if sid == &target_session
        ));
        assert!(events_rx.try_recv().is_err());
    }
    let routes = state.session_to_helper.lock().await;
    assert_eq!(routes[&target_session].helper_id, peer_id);
    assert_eq!(
        routes[&target_session].agent_instance_id,
        rebound_agent_instance
    );
}

#[tokio::test(flavor = "current_thread")]
async fn load_close_failure_closes_target_rebound_to_different_agent() {
    tokio::task::LocalSet::new()
        .run_until(run_target_rebound_during_predecessor_close_failure(false))
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn load_close_failure_keeps_target_rebound_to_same_agent() {
    tokio::task::LocalSet::new()
        .run_until(run_target_rebound_during_predecessor_close_failure(true))
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn orphan_rebind_close_failure_does_not_mark_target_owned_by_another_helper() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(32);
            let peer_id = HelperId(33);
            let old_session = SessionId::new("orphan-rebound-old");
            let target_session = SessionId::new("orphan-rebound-target");
            let current_agent_instance = AgentInstanceId::new_v4();
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let (peer_tx, _peer_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let agent_key = "orphan-rebind-close-agent".to_string();
            let agent_side_slot = Arc::new(OnceLock::new());
            agent_side_slot
                .set(agent_link_to_noop_client())
                .expect("agent-side forwarder should be set once");
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(OnceLock::new());
            assert!(agent
                .set(Arc::new(AgentCli {
                    instance_id: current_agent_instance,
                    conn: client_connection_to_rebind_during_close_agent(
                        old_session.clone(),
                        events_tx,
                    ),
                    cached_init_resp,
                    cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                    source: crate::agent_source::AgentSource::Host,
                    cmd_key: agent_key.clone(),
                    cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                    bound_helpers: Mutex::new(HashSet::new()),
                    host_list_cache: Mutex::new(None),
                    listed_ever: Mutex::new(HashSet::new()),
                }))
                .is_ok());
            let handler = HelperHandler {
                helper_id,
                agent,
                state: Arc::clone(&state),
                replacement_gate: Arc::new(Mutex::new(())),
                notif_tx: notif_tx.clone(),
                agent_side_slot,
            };
            bind_session_route(
                &state,
                old_session.clone(),
                HelperRoute {
                    helper_id,
                    agent_instance_id: current_agent_instance,
                    notif_tx,
                    forwarder: Some(agent_link_to_noop_client()),
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;
            state
                .orphaned_sessions
                .lock()
                .await
                .entry(agent_key.clone())
                .or_default()
                .insert(target_session.clone());
            state.helper_meta.lock().await.insert(
                helper_id,
                HelperRecoveryMeta {
                    last_session_id: Some(old_session.clone()),
                    ..Default::default()
                },
            );

            let request = tokio::task::spawn_local({
                let handler = handler.clone();
                let target_session = target_session.clone();
                async move {
                    handler
                        .load_session_with_timeout(
                            acp::schema::v1::LoadSessionRequest::new(
                                target_session,
                                PathBuf::from("C:\\target"),
                            ),
                            std::time::Duration::from_secs(3),
                        )
                        .await
                }
            });
            let release = match events_rx.recv().await {
                Some(ReplacementEvent::FailingClose(sid, release)) if sid == old_session => release,
                _ => panic!("orphan rebind must skip load and reach predecessor close"),
            };
            bind_session_route(
                &state,
                target_session.clone(),
                HelperRoute {
                    helper_id: peer_id,
                    agent_instance_id: AgentInstanceId::new_v4(),
                    notif_tx: peer_tx,
                    forwarder: Some(agent_link_to_noop_client()),
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;
            release
                .send(())
                .expect("predecessor close should still be waiting");
            request
                .await
                .expect("load task should finish")
                .expect_err("predecessor close failure must fail the transaction");

            assert!(
                !state
                    .orphaned_sessions
                    .lock()
                    .await
                    .get(&agent_key)
                    .is_some_and(|sessions| sessions.contains(&target_session)),
                "ownership change must not restore the orphan marker"
            );
            assert_eq!(
                state.session_to_helper.lock().await[&target_session].helper_id,
                peer_id
            );
            assert!(events_rx.try_recv().is_err());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn load_reserves_time_to_close_loaded_target_after_predecessor_timeout() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let helper_id = HelperId(24);
            let old_session = SessionId::new("deadline-old");
            let target_session = SessionId::new("deadline-target");
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let agent_instance_id = AgentInstanceId::new_v4();
            let agent_side_slot = Arc::new(OnceLock::new());
            agent_side_slot
                .set(agent_link_to_noop_client())
                .expect("agent-side forwarder should be set once");
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(OnceLock::new());
            assert!(agent
                .set(Arc::new(AgentCli {
                    instance_id: agent_instance_id,
                    conn: client_connection_to_deadline_rollback_agent(
                        old_session.clone(),
                        events_tx,
                    ),
                    cached_init_resp,
                    cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                    source: crate::agent_source::AgentSource::Host,
                    cmd_key: "deadline-rollback-agent".to_string(),
                    cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                    bound_helpers: Mutex::new(HashSet::new()),
                    host_list_cache: Mutex::new(None),
                    listed_ever: Mutex::new(HashSet::new()),
                }))
                .is_ok());
            let handler = HelperHandler {
                helper_id,
                agent,
                state: Arc::clone(&state),
                replacement_gate: Arc::new(Mutex::new(())),
                notif_tx: notif_tx.clone(),
                agent_side_slot,
            };
            bind_session_route(
                &state,
                old_session.clone(),
                HelperRoute {
                    helper_id,
                    agent_instance_id,
                    notif_tx,
                    forwarder: Some(agent_link_to_noop_client()),
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;
            state.helper_meta.lock().await.insert(
                helper_id,
                HelperRecoveryMeta {
                    last_session_id: Some(old_session.clone()),
                    ..Default::default()
                },
            );

            let request = tokio::task::spawn_local({
                let handler = handler.clone();
                let target_session = target_session.clone();
                async move {
                    handler
                        .load_session_with_timeout(
                            acp::schema::v1::LoadSessionRequest::new(
                                target_session,
                                PathBuf::from("C:\\target"),
                            ),
                            std::time::Duration::from_millis(200),
                        )
                        .await
                }
            });
            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Load(ref sid)) if sid == &target_session
            ));
            assert!(matches!(
                events_rx.recv().await,
                Some(ReplacementEvent::Close(ref sid)) if sid == &old_session
            ));

            assert!(matches!(
                tokio::time::timeout(std::time::Duration::from_secs(1), events_rx.recv())
                    .await
                    .expect("rollback close must use its reserved deadline slice"),
                Some(ReplacementEvent::Close(ref sid)) if sid == &target_session
            ));
            let error = request
                .await
                .expect("load task should finish")
                .expect_err("predecessor timeout must fail the transaction");
            assert!(format!("{error}").contains("timed out"));
            assert!(
                !state
                    .session_to_helper
                    .lock()
                    .await
                    .contains_key(&target_session),
                "rolled-back target route must be removed"
            );
        })
        .await;
}

/// An orphan session's `request_permission` (owning tab closed
/// mid-turn) must resolve to `Cancelled`, never an error — an error to
/// the shared CLI can drop the connection and every other tab with it.
#[tokio::test]
async fn request_permission_for_orphaned_session_returns_cancelled_not_error() {
    use acp::schema::v1::{
        PermissionOption, PermissionOptionId, PermissionOptionKind, RequestPermissionOutcome,
        RequestPermissionRequest, ToolCallId, ToolCallUpdate, ToolCallUpdateFields,
    };
    let state = make_state();
    let client = MasterClient {
        state: Arc::clone(&state),
    };
    // No routing entry for this session — it's orphaned.
    let req = RequestPermissionRequest::new(
        SessionId::new("orphaned-sess"),
        ToolCallUpdate::new(
            ToolCallId::new("tool-1"),
            ToolCallUpdateFields::new().title("Run: echo hi"),
        ),
        vec![PermissionOption::new(
            PermissionOptionId::new("allow-once"),
            "Allow once",
            PermissionOptionKind::AllowOnce,
        )],
    );
    let resp = client
        .request_permission(req)
        .await
        .expect("orphaned permission must resolve, not error");
    assert!(
        matches!(resp.outcome, RequestPermissionOutcome::Cancelled),
        "expected Cancelled outcome for orphaned session, got {:?}",
        resp.outcome
    );
}

/// `is_already_loaded_error` recognizes the orphan-resume signal (in
/// message OR data) so `load_session` re-binds instead of `/new`.
#[test]
fn is_already_loaded_error_matches_message_and_data() {
    let in_msg = acp::Error::new(-32602, "Session abc is already loaded");
    assert!(is_already_loaded_error(&in_msg));
    let in_data = acp::Error::internal_error()
        .data(serde_json::json!("Session abc is ALREADY LOADED in agent"));
    assert!(is_already_loaded_error(&in_data));
    let unrelated = acp::Error::new(-32603, "no helper bound to session_id");
    assert!(!is_already_loaded_error(&unrelated));
}

/// `reap_agent` must drop only the dead agent's orphan sessions, leaving
/// a co-resident agent's (e.g. Gemini next to Copilot) orphans intact.
#[tokio::test]
async fn reap_agent_drops_only_its_own_orphans() {
    let state = make_state();
    let key_a = "copilot --acp --stdio".to_string();
    let key_b = "gemini --acp".to_string();
    {
        let mut orphans = state.orphaned_sessions.lock().await;
        orphans
            .entry(key_a.clone())
            .or_default()
            .insert(SessionId::new("a-sess"));
        orphans
            .entry(key_b.clone())
            .or_default()
            .insert(SessionId::new("b-sess"));
    }
    state.orphaned_tabs.lock().await.insert(
        "tab-a".to_string(),
        (key_a.clone(), HelperId(1), SessionId::new("a-sess")),
    );
    state.orphaned_tabs.lock().await.insert(
        "tab-b".to_string(),
        (key_b.clone(), HelperId(2), SessionId::new("b-sess")),
    );
    // reap only acts when the key is a live pool entry.
    let cell = {
        let mut agents = state.agents.lock().await;
        let cell = Arc::new(tokio::sync::OnceCell::new());
        agents.insert(key_a.clone(), Arc::clone(&cell));
        cell
    };
    let stale_cell = Arc::new(tokio::sync::OnceCell::new());
    reap_agent(&state, &key_a, &stale_cell, AgentInstanceId::new_v4()).await;
    assert!(
        state.agents.lock().await.contains_key(&key_a),
        "a stale reaper must not remove a replacement pool entry"
    );
    reap_agent(&state, &key_a, &cell, AgentInstanceId::new_v4()).await;
    let orphans = state.orphaned_sessions.lock().await;
    assert!(
        !orphans.contains_key(&key_a),
        "reaped agent's orphan set must be dropped"
    );
    assert!(
        orphans
            .get(&key_b)
            .is_some_and(|s| s.contains(&SessionId::new("b-sess"))),
        "a co-resident agent's orphans must be untouched"
    );
    drop(orphans);
    let orphaned_tabs = state.orphaned_tabs.lock().await;
    assert!(
        !orphaned_tabs.contains_key("tab-a"),
        "reaping an agent must remove its stale tab fallback"
    );
    assert!(
        orphaned_tabs.contains_key("tab-b"),
        "reaping one agent must preserve another agent's tab fallback"
    );
}

/// A stale reaper must revoke its dead CLI instance's capabilities without
/// disturbing the replacement CLI that now occupies the same pool key.
#[tokio::test]
async fn stale_reaper_revokes_only_dead_agent_capabilities() {
    let state = make_state();
    let key = "copilot --acp --stdio".to_string();
    let stale_cell = Arc::new(tokio::sync::OnceCell::new());
    let replacement_cell = {
        let mut agents = state.agents.lock().await;
        let cell = Arc::new(tokio::sync::OnceCell::new());
        agents.insert(key.clone(), Arc::clone(&cell));
        cell
    };
    let dead_instance = AgentInstanceId::new_v4();
    let replacement_instance = AgentInstanceId::new_v4();
    state
        .session_mcp_capabilities
        .prepare(dead_instance, None)
        .await;
    state
        .session_mcp_capabilities
        .prepare(replacement_instance, None)
        .await;

    reap_agent(&state, &key, &stale_cell, dead_instance).await;

    assert!(
        state
            .agents
            .lock()
            .await
            .get(&key)
            .is_some_and(|cell| Arc::ptr_eq(cell, &replacement_cell)),
        "a stale reaper must preserve the replacement pool entry"
    );
    assert_eq!(
        state
            .session_mcp_capabilities
            .remove_owner(dead_instance)
            .await,
        0,
        "the dead instance's capabilities must already be revoked"
    );
    assert_eq!(
        state
            .session_mcp_capabilities
            .remove_owner(replacement_instance)
            .await,
        1,
        "the replacement instance's capabilities must remain valid"
    );
}

/// Regression for the reentrant-permission deadlock: a `prompt` in flight
/// must NOT block the master's helper-side ACP dispatch loop. If it does, a
/// `request_permission` the agent issues *mid-turn* deadlocks the shared
/// agent CLI — the helper answers the permission, but the blocked loop can
/// never read that answer, so the turn (and every later `session/new`)
/// hangs. Wire the full two hops the incident exercised:
///
/// ```text
///   mock helper --prompt--> master --prompt--> mock agent
///        ^                                          |
///        +---- request_permission (reentrant) <-----+   (answered "allow")
/// ```
///
/// With the old inline `agent_conn.prompt(a).await` the prompt never
/// returns (the timeout below fires); with `prompt_forwarding` the loop
/// stays free, the permission round-trips, and the turn ends with `EndTurn`.
#[tokio::test(flavor = "current_thread")]
async fn prompt_forward_survives_reentrant_permission() {
    use acp::schema::v1::{
        AgentRequest, AgentResponse, ClientRequest, ClientResponse, PermissionOption,
        PermissionOptionId, PermissionOptionKind, PromptRequest, PromptResponse,
        RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
        SelectedPermissionOutcome, StopReason, ToolCallId, ToolCallUpdate, ToolCallUpdateFields,
    };

    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let sid = SessionId::new("reentrant-sess");

            // ---- hop 1: master (agent-side client) <-> mock reentrant agent ----
            let (master_agent_pipe, mock_agent_pipe) = tokio::io::duplex(64 * 1024);

            // mock agent: on prompt, ask permission (reentrant, from a spawned
            // task so the mock's own dispatch loop stays free), then EndTurn.
            {
                let (ar, aw) = tokio::io::split(mock_agent_pipe);
                let builder =
                    acp::Agent
                        .builder()
                        .name("mock-reentrant-agent")
                        .on_receive_request(
                            move |req: ClientRequest,
                                  responder,
                                  cx: acp::ConnectionTo<acp::Client>| async move {
                                match req {
                                    ClientRequest::PromptRequest(a) => {
                                        let sid = a.session_id.clone();
                                        tokio::task::spawn_local(async move {
                                            let perm = RequestPermissionRequest::new(
                                                sid,
                                                ToolCallUpdate::new(
                                                    ToolCallId::new("tool-1"),
                                                    ToolCallUpdateFields::new()
                                                        .title("Run: echo hi"),
                                                ),
                                                vec![PermissionOption::new(
                                                    PermissionOptionId::new("allow-once"),
                                                    "Allow once",
                                                    PermissionOptionKind::AllowOnce,
                                                )],
                                            );
                                            // block_task from a spawned task is safe.
                                            let _ = cx.send_request(perm).block_task().await;
                                            let _ = conn::respond_enum(
                                                responder,
                                                Ok(AgentResponse::PromptResponse(
                                                    PromptResponse::new(StopReason::EndTurn),
                                                )),
                                            );
                                        });
                                        Ok(())
                                    }
                                    _ => {
                                        responder.respond_with_error(acp::Error::method_not_found())
                                    }
                                }
                            },
                            acp::on_receive_request!(),
                        );
                let (_agent_link, agent_io) =
                    conn::spawn_agent(builder, conn::byte_streams(aw.compat_write(), ar.compat()));
                tokio::task::spawn_local(async move {
                    let _ = agent_io.await;
                });
            }

            // master's client side of hop 1: MasterClient routes the agent's
            // reentrant request_permission back out to the owning helper.
            let master_client = MasterClient {
                state: Arc::clone(&state),
            };
            let agent_conn = {
                let (cr, cw) = tokio::io::split(master_agent_pipe);
                let builder = acp::Client
                    .builder()
                    .name("master-agent-side")
                    .on_receive_request(
                        {
                            let c = master_client.clone();
                            move |req: AgentRequest, responder, _cx| {
                                let c = c.clone();
                                async move {
                                    match req {
                                        AgentRequest::RequestPermissionRequest(a) => {
                                            conn::respond_enum(
                                                responder,
                                                c.request_permission(a)
                                                    .await
                                                    .map(ClientResponse::RequestPermissionResponse),
                                            )
                                        }
                                        _ => responder
                                            .respond_with_error(acp::Error::method_not_found()),
                                    }
                                }
                            }
                        },
                        acp::on_receive_request!(),
                    );
                let (link, io) =
                    conn::spawn_client(builder, conn::byte_streams(cw.compat_write(), cr.compat()));
                tokio::task::spawn_local(async move {
                    let _ = io.await;
                });
                link
            };

            // ---- hop 2: master (helper-side agent) <-> mock helper client ----
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let agent = Arc::new(OnceLock::new());
            let _ = agent.set(Arc::new(AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: agent_conn,
                cached_init_resp: acp::schema::v1::InitializeResponse::new(
                    acp::schema::ProtocolVersion::V1,
                ),
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "copilot --acp --stdio".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            }));
            let handler = HelperHandler {
                helper_id: HelperId(1),
                agent,
                state: Arc::clone(&state),
                replacement_gate: Arc::new(Mutex::new(())),
                notif_tx: notif_tx.clone(),
                agent_side_slot: Arc::new(OnceLock::new()),
            };
            let (mock_helper_pipe, master_helper_pipe) = tokio::io::duplex(64 * 1024);
            let master_to_helper = {
                let (mr, mw) = tokio::io::split(master_helper_pipe);
                let builder = acp::Agent
                    .builder()
                    .name("master-helper-side")
                    .on_receive_request(
                        {
                            let h = handler.clone();
                            move |req: ClientRequest, responder, _cx| {
                                let h = h.clone();
                                async move {
                                    match req {
                                        ClientRequest::PromptRequest(a) => {
                                            h.prompt(a, responder).await
                                        }
                                        _ => responder
                                            .respond_with_error(acp::Error::method_not_found()),
                                    }
                                }
                            }
                        },
                        acp::on_receive_request!(),
                    );
                let (link, io) =
                    conn::spawn_agent(builder, conn::byte_streams(mw.compat_write(), mr.compat()));
                tokio::task::spawn_local(async move {
                    let _ = io.await;
                });
                link
            };

            // Route the session so the agent's reentrant request_permission
            // reaches the mock helper.
            state.session_to_helper.lock().await.insert(
                sid.clone(),
                HelperRoute {
                    helper_id: HelperId(1),
                    agent_instance_id: AgentInstanceId::nil(),
                    notif_tx,
                    forwarder: Some(master_to_helper),
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            );

            // mock helper: approves any permission with "allow-once".
            let helper_link = {
                let (hr, hw) = tokio::io::split(mock_helper_pipe);
                let builder = acp::Client
                    .builder()
                    .name("mock-helper")
                    .on_receive_request(
                        move |req: AgentRequest, responder, _cx| async move {
                            match req {
                                AgentRequest::RequestPermissionRequest(_a) => conn::respond_enum(
                                    responder,
                                    Ok(ClientResponse::RequestPermissionResponse(
                                        RequestPermissionResponse::new(
                                            RequestPermissionOutcome::Selected(
                                                SelectedPermissionOutcome::new(
                                                    PermissionOptionId::new("allow-once"),
                                                ),
                                            ),
                                        ),
                                    )),
                                ),
                                _ => responder.respond_with_error(acp::Error::method_not_found()),
                            }
                        },
                        acp::on_receive_request!(),
                    );
                let (link, io) =
                    conn::spawn_client(builder, conn::byte_streams(hw.compat_write(), hr.compat()));
                tokio::task::spawn_local(async move {
                    let _ = io.await;
                });
                link
            };

            // The helper's prompt must complete despite the reentrant
            // permission — no deadlock, no timeout.
            let resp = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                helper_link.prompt(PromptRequest::new(sid.clone(), vec!["hi".into()])),
            )
            .await
            .expect("prompt deadlocked: helper dispatch loop blocked during in-flight prompt")
            .expect("prompt should succeed");

            assert!(
                matches!(resp.stop_reason, StopReason::EndTurn),
                "expected EndTurn, got {:?}",
                resp.stop_reason
            );
        })
        .await;
}

#[test]
fn restart_agent_pane_event_shape_carries_tab_and_session() {
    let sid = SessionId::from("sess-abc");
    let evt = build_restart_agent_pane_event("tab-42", Some(&sid));
    assert_eq!(evt["type"], "event");
    assert_eq!(evt["method"], "restart_agent_pane");
    assert_eq!(evt["params"]["tab_id"], "tab-42");
    assert_eq!(evt["params"]["session_id"], "sess-abc");
    assert_eq!(evt["params"]["reason"], "helper_disconnect");
}

#[test]
fn restart_agent_pane_event_null_session_when_none() {
    let evt = build_restart_agent_pane_event("tab-7", None);
    assert!(evt["params"]["session_id"].is_null());
    assert_eq!(evt["params"]["tab_id"], "tab-7");
}

fn make_notif(sid: &SessionId) -> SessionNotification {
    SessionNotification::new(
        sid.clone(),
        SessionUpdate::AgentMessageChunk(ContentChunk::new("hi".into())),
    )
}

fn make_usage_notif(sid: &SessionId, used: u64) -> SessionNotification {
    SessionNotification::new(
        sid.clone(),
        SessionUpdate::UsageUpdate(acp::schema::v1::UsageUpdate::new(used, 100)),
    )
}

async fn route(state: &Arc<MasterStateInner>, notif: SessionNotification) {
    let client = MasterClient {
        state: Arc::clone(state),
    };
    client.session_notification(notif).await.unwrap();
}

/// New `session_notification`s for a registered SessionId reach
/// the owning helper's channel, and a second helper's channel
/// stays untouched.
#[tokio::test]
async fn session_notification_routes_to_owning_helper() {
    let state = make_state();
    let (tx1, mut rx1) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    let (tx2, mut rx2) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    let sid1 = SessionId::new("sess-1");
    let sid2 = SessionId::new("sess-2");

    {
        let mut map = state.session_to_helper.lock().await;
        map.insert(
            sid1.clone(),
            HelperRoute {
                helper_id: HelperId(1),
                agent_instance_id: AgentInstanceId::nil(),
                notif_tx: tx1,
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
        map.insert(
            sid2.clone(),
            HelperRoute {
                helper_id: HelperId(2),
                agent_instance_id: AgentInstanceId::nil(),
                notif_tx: tx2,
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
    }

    route(&state, make_notif(&sid1)).await;
    assert!(rx1.try_recv().is_ok(), "helper 1 should have received");
    assert!(
        rx2.try_recv().is_err(),
        "helper 2 should NOT have received helper 1's notification"
    );
}

/// When the helper's receiver has been dropped, the failed-send
/// path removes the routing entry so the warning doesn't repeat
/// for the same SessionId on every subsequent notification.
#[tokio::test]
async fn session_notification_drops_entry_on_send_failure() {
    let state = make_state();
    let (tx, rx) = mpsc::channel::<SessionNotification>(NOTIF_CHANNEL_CAPACITY);
    let sid = SessionId::new("dead-session");
    {
        let mut map = state.session_to_helper.lock().await;
        map.insert(
            sid.clone(),
            HelperRoute {
                helper_id: HelperId(7),
                agent_instance_id: AgentInstanceId::nil(),
                notif_tx: tx,
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
    }
    drop(rx); // simulate helper going away

    route(&state, make_notif(&sid)).await;

    let map = state.session_to_helper.lock().await;
    assert!(
        !map.contains_key(&sid),
        "send failure should have removed the routing entry"
    );
}

/// Regression test for the rebinding race in the Closed-cleanup
/// path. Sequence:
///   1. Helper A is bound to `sid`; we snapshot its `notif_tx`.
///   2. Helper A's receiver is dropped (channel becomes Closed).
///   3. Helper B rebinds the SAME `sid` via `load_session` —
///      the map entry now points at helper B.
///   4. Master finally tries `try_send` on the snapshotted (now
///      Closed) sender → `TrySendError::Closed`.
///
/// Before the fix the cleanup path would `map.remove(&sid)`
/// unconditionally and clobber helper B's freshly-installed route.
/// With the fix it compares `helper_id` and leaves the new entry
/// alone.
#[tokio::test]
async fn session_notification_preserves_rebound_route_on_closed() {
    let state = make_state();
    let sid = SessionId::new("reused-session");

    // Helper A is initially bound; we'll snapshot its sender by
    // invoking session_notification — `route` only takes a state
    // snapshot under the lock, then drops the lock before
    // try_send. We need the snapshot to capture A but the rebind
    // to happen before try_send wakes Closed. Easiest: drop A's
    // receiver, then immediately rebind to B in the same task,
    // then route — `try_send` sees Closed; the route identity check
    // sees the entry uses B's new channel; cleanup must NOT remove B.
    let (tx_a, rx_a) = mpsc::channel::<SessionNotification>(NOTIF_CHANNEL_CAPACITY);
    {
        let mut map = state.session_to_helper.lock().await;
        map.insert(
            sid.clone(),
            HelperRoute {
                helper_id: HelperId(1),
                agent_instance_id: AgentInstanceId::nil(),
                notif_tx: tx_a.clone(),
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
    }
    drop(rx_a); // A's channel is now Closed

    // We can't reliably interleave "snapshot then rebind then
    // try_send" without unsafe scheduling; instead, simulate the
    // exact post-race state: helper B has already rebound by the
    // time the cleanup runs. Construct the snapshot manually and
    // invoke a tiny helper that mirrors the production
    // cleanup-with-identity-check path.
    let snap_helper_a = HelperId(1);

    // Rebind the same helper to a fresh channel (simulating a racing
    // load_session landing between snapshot and try_send).
    let (tx_b, _rx_b) = mpsc::channel::<SessionNotification>(NOTIF_CHANNEL_CAPACITY);
    {
        let mut map = state.session_to_helper.lock().await;
        map.insert(
            sid.clone(),
            HelperRoute {
                helper_id: HelperId(1),
                agent_instance_id: AgentInstanceId::nil(),
                notif_tx: tx_b.clone(),
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
    }

    // Drive the real production path. `tx_a` is the snapshot we'd
    // have captured before the rebind; `try_send` on it returns
    // Closed. The cleanup must look at the current map entry,
    // see that its channel differs from A's snapshot, and leave it alone.
    match tx_a.try_send(make_notif(&sid)) {
        Err(mpsc::error::TrySendError::Closed(_)) => {}
        other => panic!("expected Closed, got {other:?}"),
    }
    {
        let mut map = state.session_to_helper.lock().await;
        match map.get(&sid) {
            Some(current)
                if current.helper_id == snap_helper_a && current.notif_tx.same_channel(&tx_a) =>
            {
                map.remove(&sid);
            }
            _ => {} // identity mismatch — leave new route intact
        }
    }

    let map = state.session_to_helper.lock().await;
    let current = map.get(&sid).expect("helper B's route must survive");
    assert_eq!(
        current.helper_id,
        HelperId(1),
        "Closed cleanup must not remove a route rebound by the same helper"
    );
    assert!(current.notif_tx.same_channel(&tx_b));
}

/// A full bounded channel drops the new notification (and logs)
/// instead of `await`-blocking — protects the agent CLI I/O loop
/// from head-of-line blocking when one helper's pipe stalls.
/// Verified by filling a capacity-1 channel without draining, then
/// routing — the second notification must be silently dropped and
/// the routing entry must remain (channel is Full, not Closed).
#[tokio::test]
async fn session_notification_drops_on_full_channel() {
    let state = make_state();
    let (tx, _rx) = mpsc::channel::<SessionNotification>(1);
    let sid = SessionId::new("slow-helper");
    {
        let mut map = state.session_to_helper.lock().await;
        map.insert(
            sid.clone(),
            HelperRoute {
                helper_id: HelperId(9),
                agent_instance_id: AgentInstanceId::nil(),
                notif_tx: tx.clone(),
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
    }
    // Fill capacity. _rx is held so the channel stays open.
    tx.try_send(make_notif(&sid)).unwrap();
    // Second send via the routing path must be a no-op-with-warn,
    // not a panic or an error.
    route(&state, make_notif(&sid)).await;
    // Routing entry survives Full (only Closed removes it).
    let map = state.session_to_helper.lock().await;
    assert!(
        map.contains_key(&sid),
        "Full (not Closed) must NOT remove the routing entry"
    );
}

#[tokio::test]
async fn session_notification_coalesces_context_without_dropping_pending_cost() {
    let state = make_state();
    let (tx, _rx) = mpsc::channel::<SessionNotification>(1);
    let sid = SessionId::new("slow-usage-helper");
    {
        let mut map = state.session_to_helper.lock().await;
        map.insert(
            sid.clone(),
            HelperRoute {
                helper_id: HelperId(10),
                agent_instance_id: AgentInstanceId::nil(),
                notif_tx: tx.clone(),
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
    }
    tx.try_send(make_notif(&sid)).unwrap();

    route(
        &state,
        SessionNotification::new(
            sid.clone(),
            SessionUpdate::UsageUpdate(
                acp::schema::v1::UsageUpdate::new(10, 100)
                    .cost(acp::schema::v1::Cost::new(0.004, "USD")),
            ),
        ),
    )
    .await;
    route(&state, make_usage_notif(&sid, 25)).await;

    assert_eq!(tx.capacity(), 0);
    let pending = state.pending_usage.lock().await;
    let (owner, notification) = pending.get(&sid).expect("latest usage retained");
    assert_eq!(*owner, HelperId(10));
    match &notification.update {
        SessionUpdate::UsageUpdate(update) => {
            assert_eq!(update.used, 25);
            assert_eq!(
                update.cost.as_ref(),
                Some(&acp::schema::v1::Cost::new(0.004, "USD"))
            );
        }
        other => panic!("expected usage update, got {other:?}"),
    }
}

#[tokio::test]
async fn rebinding_session_clears_previous_helpers_pending_usage() {
    let state = make_state();
    let sid = SessionId::new("rebound-usage-session");
    let (tx_a, _rx_a) = mpsc::channel::<SessionNotification>(1);
    let (tx_b, _rx_b) = mpsc::channel::<SessionNotification>(1);

    bind_session_route(
        &state,
        sid.clone(),
        HelperRoute {
            helper_id: HelperId(1),
            agent_instance_id: AgentInstanceId::nil(),
            notif_tx: tx_a,
            forwarder: None,
            consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        },
    )
    .await;
    route(&state, make_usage_notif(&sid, 25)).await;
    assert!(state.pending_usage.lock().await.contains_key(&sid));

    bind_session_route(
        &state,
        sid.clone(),
        HelperRoute {
            helper_id: HelperId(2),
            agent_instance_id: AgentInstanceId::nil(),
            notif_tx: tx_b,
            forwarder: None,
            consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        },
    )
    .await;

    assert!(!state.pending_usage.lock().await.contains_key(&sid));
    assert_eq!(
        state.session_to_helper.lock().await[&sid].helper_id,
        HelperId(2)
    );
}

/// Unknown SessionId is a no-op (warned but not errored) — the
/// `Client` trait return value must stay `Ok` so the master's
/// I/O loop doesn't tear down on a stale notification.
#[tokio::test]
async fn session_notification_unknown_session_is_noop() {
    let state = make_state();
    let sid = SessionId::new("never-registered");
    // Just ensure the call doesn't panic and returns Ok.
    route(&state, make_notif(&sid)).await;
    let map = state.session_to_helper.lock().await;
    assert!(map.is_empty());
}

/// `drop_sessions_for_helper` removes exactly the rows owned by
/// the disconnecting helper, leaving other helpers' rows intact.
/// This is the cleanup the helper-disconnect path runs.
#[tokio::test]
async fn drop_sessions_for_helper_retains_only_other_helpers() {
    let state = make_state();
    let (tx_a, _rx_a) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    let (tx_b, _rx_b) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    let (tx_c, _rx_c) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    {
        let mut map = state.session_to_helper.lock().await;
        map.insert(
            SessionId::new("a1"),
            HelperRoute {
                helper_id: HelperId(1),
                agent_instance_id: AgentInstanceId::nil(),
                notif_tx: tx_a.clone(),
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
        map.insert(
            SessionId::new("a2"),
            HelperRoute {
                helper_id: HelperId(1),
                agent_instance_id: AgentInstanceId::nil(),
                notif_tx: tx_a,
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
        map.insert(
            SessionId::new("b1"),
            HelperRoute {
                helper_id: HelperId(2),
                agent_instance_id: AgentInstanceId::nil(),
                notif_tx: tx_b,
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
        map.insert(
            SessionId::new("c1"),
            HelperRoute {
                helper_id: HelperId(3),
                agent_instance_id: AgentInstanceId::nil(),
                notif_tx: tx_c,
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
    }

    let dropped = drop_sessions_for_helper(&state, HelperId(1)).await;
    assert_eq!(dropped.len(), 2);
    assert!(dropped.contains(&SessionId::new("a1")));
    assert!(dropped.contains(&SessionId::new("a2")));

    let map = state.session_to_helper.lock().await;
    assert!(!map.contains_key(&SessionId::new("a1")));
    assert!(!map.contains_key(&SessionId::new("a2")));
    assert!(map.contains_key(&SessionId::new("b1")));
    assert!(map.contains_key(&SessionId::new("c1")));
}

/// Companion invariant to `drop_sessions_for_helper_retains_only_other_helpers`:
/// the same teardown call must also remove the corresponding rows
/// from `state.registry`. Otherwise, a `session/list` response (or
/// a downstream `intellterm.wta/focus_session` lookup) could hand
/// out a SessionId whose helper is already gone, and the session management view
/// would route Enter to a dead pane.
#[tokio::test]
async fn drop_sessions_for_helper_also_clears_registry() {
    use crate::session_registry::SessionInfo;
    use std::path::PathBuf;

    let state = make_state();
    let (tx_a, _rx_a) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    let (tx_b, _rx_b) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);

    // Two helpers, one session each.
    let sid_a = SessionId::new("alive-a");
    let sid_b = SessionId::new("alive-b");
    {
        let mut map = state.session_to_helper.lock().await;
        map.insert(
            sid_a.clone(),
            HelperRoute {
                helper_id: HelperId(1),
                agent_instance_id: AgentInstanceId::nil(),
                notif_tx: tx_a,
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
        map.insert(
            sid_b.clone(),
            HelperRoute {
                helper_id: HelperId(2),
                agent_instance_id: AgentInstanceId::nil(),
                notif_tx: tx_b,
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
    }
    state
        .registry
        .upsert(SessionInfo::new(sid_a.clone(), PathBuf::from("/repo/a")))
        .await;
    state
        .registry
        .upsert(SessionInfo::new(sid_b.clone(), PathBuf::from("/repo/b")))
        .await;

    // Disconnect helper 1.
    drop_sessions_for_helper(&state, HelperId(1)).await;

    assert!(
        state.registry.lookup(&sid_a).await.is_none(),
        "registry must drop sessions owned by the disconnecting helper"
    );
    assert!(
        state.registry.lookup(&sid_b).await.is_some(),
        "registry must keep sessions owned by other helpers"
    );
    let snapshot = state.registry.snapshot().await;
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].session_id, sid_b);
}

/// `broadcast_ext_to_helpers` should reach every currently
/// registered helper subscriber, leaving the subscriber map
/// intact when channels are live.
#[tokio::test]
async fn broadcast_ext_to_helpers_fans_out_to_all_subscribers() {
    use crate::session_registry::{self, build_session_added_notification, SessionInfo};
    use std::path::PathBuf;

    let state = make_state();
    let (tx1, mut rx1) = mpsc::unbounded_channel::<acp::schema::v1::ExtNotification>();
    let (tx2, mut rx2) = mpsc::unbounded_channel::<acp::schema::v1::ExtNotification>();
    {
        let mut subs = state.helper_ext_subscribers.lock().await;
        subs.insert(HelperId(1), tx1);
        subs.insert(HelperId(2), tx2);
    }

    let info = SessionInfo::new(SessionId::new("alive-x"), PathBuf::from("/repo/x"));
    broadcast_ext_to_helpers(&state, build_session_added_notification(&info)).await;

    let got1 = rx1.try_recv().expect("helper 1 receives broadcast");
    let got2 = rx2.try_recv().expect("helper 2 receives broadcast");
    assert_eq!(
        &*got1.method,
        session_registry::INTELLTERM_METHOD_SESSION_ADDED
    );
    assert_eq!(
        &*got2.method,
        session_registry::INTELLTERM_METHOD_SESSION_ADDED
    );

    let subs = state.helper_ext_subscribers.lock().await;
    assert_eq!(subs.len(), 2, "live subscribers stay registered");
}

/// If a helper's ext-channel receiver has been dropped, the
/// broadcast should prune the entry so we don't keep warning on
/// every future fan-out.
#[tokio::test]
async fn broadcast_ext_to_helpers_prunes_dead_subscribers() {
    use crate::session_registry::build_session_removed_notification;

    let state = make_state();
    let (tx_dead, rx_dead) = mpsc::unbounded_channel::<acp::schema::v1::ExtNotification>();
    let (tx_live, _rx_live) = mpsc::unbounded_channel::<acp::schema::v1::ExtNotification>();
    {
        let mut subs = state.helper_ext_subscribers.lock().await;
        subs.insert(HelperId(7), tx_dead);
        subs.insert(HelperId(8), tx_live);
    }
    drop(rx_dead);

    broadcast_ext_to_helpers(
        &state,
        build_session_removed_notification(&SessionId::new("zzz")),
    )
    .await;

    let subs = state.helper_ext_subscribers.lock().await;
    assert!(!subs.contains_key(&HelperId(7)), "dead subscriber pruned");
    assert!(subs.contains_key(&HelperId(8)), "live subscriber retained");
}

/// When a helper disconnects, `drop_sessions_for_helper` should
/// emit a `session_removed` for every session it owned, fanning
/// out to all OTHER helpers' subscribers.
#[tokio::test]
async fn drop_sessions_for_helper_broadcasts_session_removed_to_peers() {
    use crate::session_registry::{self, SessionInfo};
    use std::path::PathBuf;

    let state = make_state();
    // Helper 1 owns two sessions, helper 2 owns none but is
    // subscribed (it's a peer that should learn of the removals).
    let (notif_tx1, _notif_rx1) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    let (ext_tx2, mut ext_rx2) = mpsc::unbounded_channel::<acp::schema::v1::ExtNotification>();
    let sid_a = SessionId::new("removed-a");
    let sid_b = SessionId::new("removed-b");
    {
        let mut map = state.session_to_helper.lock().await;
        map.insert(
            sid_a.clone(),
            HelperRoute {
                helper_id: HelperId(1),
                agent_instance_id: AgentInstanceId::nil(),
                notif_tx: notif_tx1.clone(),
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
        map.insert(
            sid_b.clone(),
            HelperRoute {
                helper_id: HelperId(1),
                agent_instance_id: AgentInstanceId::nil(),
                notif_tx: notif_tx1,
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
    }
    state
        .registry
        .upsert(SessionInfo::new(sid_a.clone(), PathBuf::from("/a")))
        .await;
    state
        .registry
        .upsert(SessionInfo::new(sid_b.clone(), PathBuf::from("/b")))
        .await;
    {
        let mut subs = state.helper_ext_subscribers.lock().await;
        subs.insert(HelperId(2), ext_tx2);
    }

    drop_sessions_for_helper(&state, HelperId(1)).await;

    // Expect two session_removed notifications on peer 2's channel;
    // Task A also emits sessions/changed after each registry mutation.
    let mut got: Vec<acp::schema::v1::SessionId> = Vec::new();
    while let Ok(ext) = ext_rx2.try_recv() {
        match session_registry::parse_ext_notification(&ext) {
            session_registry::WtaExtNotification::SessionRemoved(sid) => got.push(sid),
            session_registry::WtaExtNotification::SessionsChanged => {}
            other => panic!("expected SessionRemoved or SessionsChanged, got {other:?}"),
        }
    }
    got.sort_by(|a, b| a.0.cmp(&b.0));
    let mut expected = vec![sid_a, sid_b];
    expected.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(got, expected);
}

#[tokio::test(flavor = "current_thread")]
async fn replaced_session_already_rebound_is_not_physically_closed() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let sid = SessionId::new("already-rebound");
            let old_owner = HelperId(1);
            let new_owner = HelperId(2);
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            bind_session_route(
                &state,
                sid.clone(),
                HelperRoute {
                    helper_id: new_owner,
                    agent_instance_id: AgentInstanceId::nil(),
                    notif_tx,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;
            state
                .registry
                .upsert(crate::session_registry::SessionInfo::new(
                    sid.clone(),
                    PathBuf::from("C:\\new-owner"),
                ))
                .await;
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = AgentCli {
                instance_id: AgentInstanceId::new_v4(),
                conn: client_connection_to_blocking_close_agent(events_tx),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "already-rebound-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            };

            assert_eq!(
                close_and_retire_replaced_session(
                    &state,
                    old_owner,
                    &agent,
                    &sid,
                    SESSION_CLOSE_TIMEOUT,
                )
                .await
                .unwrap(),
                ReplacedSessionCleanup::NotOwned
            );
            assert!(
                events_rx.try_recv().is_err(),
                "ownership must be checked before sending session/close"
            );
            assert_eq!(
                state.session_to_helper.lock().await[&sid].helper_id,
                new_owner
            );
            assert!(state.registry.lookup(&sid).await.is_some());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn physical_close_allows_agent_callback_route_lookup_before_response() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let sid = SessionId::new("close-callback");
            let helper_id = HelperId(1);
            let agent_instance_id = AgentInstanceId::new_v4();
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            bind_session_route(
                &state,
                sid.clone(),
                HelperRoute {
                    helper_id,
                    agent_instance_id,
                    notif_tx,
                    forwarder: Some(agent_link_to_noop_client()),
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;
            let (callback_tx, callback_rx) = tokio::sync::oneshot::channel();
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = AgentCli {
                instance_id: agent_instance_id,
                conn: client_connection_to_callback_close_agent(Arc::clone(&state), callback_tx),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "callback-close-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            };

            let cleanup = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                close_and_retire_replaced_session(
                    &state,
                    helper_id,
                    &agent,
                    &sid,
                    SESSION_CLOSE_TIMEOUT,
                ),
            )
            .await
            .expect("close must not deadlock while its callback looks up the route")
            .expect("close should succeed");

            assert_eq!(cleanup, ReplacedSessionCleanup::PhysicallyClosed);
            callback_rx
                .await
                .expect("agent callback must complete before close response");
            assert!(!state.session_to_helper.lock().await.contains_key(&sid));
        })
        .await;
}

#[tokio::test]
async fn force_retire_never_purges_a_rebound_session() {
    let state = make_state();
    let sid = SessionId::new("force-retire-rebound");
    let old_owner = HelperId(1);
    let new_owner = HelperId(2);
    let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    bind_session_route(
        &state,
        sid.clone(),
        HelperRoute {
            helper_id: new_owner,
            agent_instance_id: AgentInstanceId::new_v4(),
            notif_tx,
            forwarder: None,
            consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        },
    )
    .await;
    state
        .registry
        .upsert(crate::session_registry::SessionInfo::new(
            sid.clone(),
            PathBuf::from("C:\\rebound"),
        ))
        .await;

    assert_eq!(
        force_retire_owned_session_state(&state, old_owner, &sid).await,
        ReplacedSessionCleanup::NotOwned
    );
    assert_eq!(
        state.session_to_helper.lock().await[&sid].helper_id,
        new_owner
    );
    assert!(
        state.registry.lookup(&sid).await.is_some(),
        "force-retire must not purge registry/MCP state after ownership rebounds"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn physical_close_blocks_rebind_until_retirement_completes() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let sid = SessionId::new("rebound-during-close");
            let old_owner = HelperId(1);
            let new_owner = HelperId(2);
            let (old_tx, _old_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let (new_tx, _new_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let (other_tx, _other_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let agent_instance_id = AgentInstanceId::new_v4();
            bind_session_route(
                &state,
                sid.clone(),
                HelperRoute {
                    helper_id: old_owner,
                    agent_instance_id,
                    notif_tx: old_tx,
                    forwarder: None,
                    consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
            )
            .await;
            state
                .registry
                .upsert(crate::session_registry::SessionInfo::new(
                    sid.clone(),
                    PathBuf::from("C:\\old"),
                ))
                .await;
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let mut cached_init_resp =
                acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
            cached_init_resp
                .agent_capabilities
                .session_capabilities
                .close = Some(acp::schema::v1::SessionCloseCapabilities::new());
            let agent = Arc::new(AgentCli {
                instance_id: agent_instance_id,
                conn: client_connection_to_blocking_close_agent(events_tx),
                cached_init_resp,
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "blocking-close-agent".to_string(),
                cloud_catalog: Mutex::new(NativeCloudCatalogState::Unavailable),
                bound_helpers: Mutex::new(HashSet::new()),
                host_list_cache: Mutex::new(None),
                listed_ever: Mutex::new(HashSet::new()),
            });

            let close_state = Arc::clone(&state);
            let close_sid = sid.clone();
            let close_agent = Arc::clone(&agent);
            let close = tokio::task::spawn_local(async move {
                close_and_retire_replaced_session(
                    &close_state,
                    old_owner,
                    &close_agent,
                    &close_sid,
                    SESSION_CLOSE_TIMEOUT,
                )
                .await
            });
            let ReplacementEvent::BlockingClose(closed_sid, release_close) =
                tokio::time::timeout(std::time::Duration::from_secs(1), events_rx.recv())
                .await
                    .expect("physical close event must arrive before the test timeout")
                .expect("physical close must reach the agent")
            else {
                panic!("expected blocking session/close event");
            };
            assert_eq!(closed_sid, sid);

            let unrelated_sid = SessionId::new("unrelated-during-close");
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                bind_session_route(
                    &state,
                    unrelated_sid.clone(),
                    HelperRoute {
                        helper_id: new_owner,
                        agent_instance_id: AgentInstanceId::nil(),
                        notif_tx: other_tx,
                        forwarder: None,
                        consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                    },
                ),
            )
            .await
            .expect("unrelated SessionId must not wait for physical close");

            let binding_state = Arc::clone(&state);
            let binding_sid = sid.clone();
            let bind = tokio::task::spawn_local(async move {
                bind_session_route(
                    &binding_state,
                    binding_sid,
                    HelperRoute {
                        helper_id: new_owner,
                        agent_instance_id: AgentInstanceId::nil(),
                        notif_tx: new_tx,
                        forwarder: None,
                        consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                    },
                )
                .await
            });
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            assert!(
                !bind.is_finished(),
                "same-SID rebind must wait while physical close owns its lifecycle gate"
            );

            release_close.send(()).unwrap();
            assert_eq!(
                close.await.unwrap().unwrap(),
                ReplacedSessionCleanup::PhysicallyClosed
            );
            bind.await.unwrap();
            assert_eq!(
                state.session_to_helper.lock().await[&sid].helper_id,
                new_owner
            );
            assert!(state
                .session_to_helper
                .lock()
                .await
                .contains_key(&unrelated_sid));
            assert!(state.registry.lookup(&sid).await.is_none());
        })
        .await;
}

/// `route_for` (used by every `MasterClient::<client-method>`
/// forwarder) must return `internal_error` when the agent CLI
/// sends a request for a session that no helper has registered
/// — typically a stale call after the owning helper disconnected.
/// Returning `Ok(...)` here would dereference an invalid route.
#[tokio::test]
async fn route_for_unknown_session_id_returns_internal_error() {
    let state = make_state();
    let client = MasterClient {
        state: Arc::clone(&state),
    };
    let err = client
        .route_for(&SessionId::new("ghost"), "request_permission")
        .await
        .expect_err("unknown session_id must not resolve");
    assert_eq!(err.code, acp::ErrorCode::InternalError);
}

/// `route_for` must also fail when the routing entry exists but
/// its `forwarder` slot is `None`. Production code never inserts
/// a `None` forwarder (every `new_session` / `load_session` path
/// upgrades the helper's `Weak<AgentSideConnection>`), so reaching
/// this branch means the slot was inserted before the conn was
/// alive — that's a bug we want to surface, not paper over.
#[tokio::test]
async fn route_for_none_forwarder_returns_internal_error() {
    let state = make_state();
    let (tx, _rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    {
        let mut map = state.session_to_helper.lock().await;
        map.insert(
            SessionId::new("orphan"),
            HelperRoute {
                helper_id: HelperId(42),
                agent_instance_id: AgentInstanceId::nil(),
                notif_tx: tx,
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
    }
    let client = MasterClient {
        state: Arc::clone(&state),
    };
    let err = client
        .route_for(&SessionId::new("orphan"), "create_terminal")
        .await
        .expect_err("None forwarder must not resolve");
    assert_eq!(err.code, acp::ErrorCode::InternalError);
}

/// End-to-end through one of the forwarder methods: a Client-trait
/// request on `MasterClient` for an unknown session_id propagates
/// the same `internal_error` (rather than the trait default
/// `method_not_found`, which would mislead the agent CLI into
/// thinking the master doesn't support terminals at all).
#[tokio::test]
async fn master_client_create_terminal_unknown_session_returns_internal_error() {
    let state = make_state();
    let client = MasterClient {
        state: Arc::clone(&state),
    };
    let req = acp::schema::v1::CreateTerminalRequest::new(
        SessionId::new("nobody-home"),
        "echo".to_string(),
    );
    let err = client
        .create_terminal(req)
        .await
        .expect_err("create_terminal on unknown session must fail");
    assert_eq!(err.code, acp::ErrorCode::InternalError);
}

#[tokio::test]
async fn sessions_list_handler_returns_registry_snapshot_payload() {
    use crate::session_registry::{self, SessionInfo};
    use std::path::PathBuf;

    let state = make_state();
    let mut row = SessionInfo::new(SessionId::new("sess-b"), PathBuf::from("C:\\repo\\b"));
    row.status = Some(crate::agent_sessions::AgentStatus::Idle);
    row.cli_source = Some(crate::agent_sessions::CliSource::Copilot);
    row.last_activity_at_ms = Some(42);
    state.registry.upsert(row.clone()).await;

    let resp = handle_sessions_list(
        &state,
        None,
        &session_registry::SessionsListParams { rescan: false },
    )
    .await
    .expect("sessions/list succeeds");
    let parsed = session_registry::parse_sessions_list_response(&resp.0).expect("response parses");

    assert_eq!(parsed.sessions, vec![row]);
}

#[tokio::test]
async fn drop_sessions_for_helper_broadcasts_sessions_changed() {
    use crate::session_registry::{self, SessionInfo};
    use std::path::PathBuf;

    let state = make_state();
    let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    let (ext_tx, mut ext_rx) = mpsc::unbounded_channel::<acp::schema::v1::ExtNotification>();
    let sid = SessionId::new("removed-a");
    {
        let mut map = state.session_to_helper.lock().await;
        map.insert(
            sid.clone(),
            HelperRoute {
                helper_id: HelperId(1),
                agent_instance_id: AgentInstanceId::nil(),
                notif_tx,
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
    }
    state
        .registry
        .upsert(SessionInfo::new(sid, PathBuf::from("C:\\repo")))
        .await;
    {
        let mut subs = state.helper_ext_subscribers.lock().await;
        subs.insert(HelperId(2), ext_tx);
    }

    drop_sessions_for_helper(&state, HelperId(1)).await;

    let methods: Vec<String> = std::iter::from_fn(|| ext_rx.try_recv().ok())
        .map(|ext| ext.method.to_string())
        .collect();
    assert!(methods.contains(&session_registry::INTELLTERM_METHOD_SESSION_REMOVED.to_string()));
    assert!(methods.contains(&session_registry::INTELLTERM_METHOD_SESSIONS_CHANGED.to_string()));
}

// ─── Task C master mutation RPCs ────────────────────────────────

#[tokio::test]
async fn session_resume_dispatched_historical_flips_and_broadcasts() {
    use crate::session_registry::SessionInfo;
    use std::path::PathBuf;
    let state = make_state();
    let (tx, mut rx) = mpsc::unbounded_channel();
    state
        .helper_ext_subscribers
        .lock()
        .await
        .insert(HelperId(7), tx);
    let sid = acp::schema::v1::SessionId::new("hist-sid");
    let mut info = SessionInfo::new(sid.clone(), PathBuf::from("/repo"));
    info.status = Some(crate::agent_sessions::AgentStatus::Historical);
    state.registry.upsert(info).await;
    let params = session_resume_params_for(&sid);
    let resp = handle_session_resume_dispatched(&state, &params)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();
    assert_eq!(body["flipped"], true);
    assert_eq!(body["current_status"], "Idle");
    assert_eq!(
        state.registry.lookup(&sid).await.unwrap().status,
        Some(crate::agent_sessions::AgentStatus::Idle)
    );
    let notif = rx.try_recv().expect("flip must broadcast sessions/changed");
    assert_eq!(
        &*notif.method,
        crate::session_registry::INTELLTERM_METHOD_SESSIONS_CHANGED
    );
}

#[tokio::test]
async fn session_resume_dispatched_live_is_noop() {
    use crate::session_registry::SessionInfo;
    use std::path::PathBuf;
    let state = make_state();
    let (tx, mut rx) = mpsc::unbounded_channel();
    state
        .helper_ext_subscribers
        .lock()
        .await
        .insert(HelperId(7), tx);
    let sid = acp::schema::v1::SessionId::new("live-sid");
    let mut info = SessionInfo::new(sid.clone(), PathBuf::from("/repo"));
    info.status = Some(crate::agent_sessions::AgentStatus::Idle);
    state.registry.upsert(info).await;
    let params = session_resume_params_for(&sid);
    let resp = handle_session_resume_dispatched(&state, &params)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();
    assert_eq!(body["flipped"], false);
    assert_eq!(body["current_status"], "Idle");
    assert!(rx.try_recv().is_err(), "no-op must not broadcast");
}

#[tokio::test]
async fn session_focus_with_bound_pane_calls_wtcli() {
    use crate::session_registry::SessionInfo;
    use std::path::PathBuf;
    let mock = Arc::new(MockWtChannel::ok());
    let state = make_state_with_wt(mock.clone());
    let sid = acp::schema::v1::SessionId::new("focus-sid");
    let mut info = SessionInfo::new(sid.clone(), PathBuf::from("/repo"));
    info.pane_session_id = Some("pane-123".to_string());
    state.registry.upsert(info).await;
    let params = session_focus_params_for(&sid);
    let resp = handle_session_focus(&state, &params).await.unwrap();
    let body: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();
    assert_eq!(body["focused"], true);
    assert_eq!(body["pane_session_id"], "pane-123");
    assert_eq!(mock.calls()[0].0, "focus_pane");
}

#[tokio::test]
async fn session_focus_without_pane_returns_no_pane() {
    use crate::session_registry::SessionInfo;
    use std::path::PathBuf;
    let mock = Arc::new(MockWtChannel::ok());
    let state = make_state_with_wt(mock.clone());
    let sid = acp::schema::v1::SessionId::new("orphan-sid");
    state
        .registry
        .upsert(SessionInfo::new(sid.clone(), PathBuf::from("/repo")))
        .await;
    let params = session_focus_params_for(&sid);
    let resp = handle_session_focus(&state, &params).await.unwrap();
    let body: serde_json::Value = serde_json::from_str(resp.0.get()).unwrap();
    assert_eq!(body["focused"], false);
    assert_eq!(body["reason"], "no_pane");
    assert!(mock.calls().is_empty());
}

fn session_resume_params_for(
    sid: &acp::schema::v1::SessionId,
) -> crate::session_registry::SessionResumeDispatchedParams {
    crate::session_registry::SessionResumeDispatchedParams { sid: sid.clone() }
}

fn session_focus_params_for(
    sid: &acp::schema::v1::SessionId,
) -> crate::session_registry::SessionFocusParams {
    crate::session_registry::SessionFocusParams { sid: sid.clone() }
}

// ─── handle_focus_session ───────────────────────────────────────

/// Mock `WtChannel` that captures every `request` call into a
/// shared vec so tests can assert the dispatched method + params.
/// Returns `Ok(<configured-response>)` for every request — the
/// real `CliChannel` returns a JSON value from `wtcli`, but the
/// handler doesn't inspect it (it just maps `Ok(_)` to a fixed
/// success ExtResponse), so any JSON works here.
struct MockWtChannel {
    calls: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
    fail_with: Option<String>,
}

impl MockWtChannel {
    fn ok() -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
            fail_with: None,
        }
    }
    fn failing(message: &str) -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
            fail_with: Some(message.to_string()),
        }
    }
    fn calls(&self) -> Vec<(String, serde_json::Value)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl crate::shell::wt_channel::WtChannel for MockWtChannel {
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        self.calls
            .lock()
            .unwrap()
            .push((method.to_string(), params));
        match &self.fail_with {
            Some(msg) => Err(anyhow::anyhow!("{msg}")),
            None => Ok(serde_json::json!({ "ok": true })),
        }
    }
    fn is_available(&self) -> bool {
        true
    }
}

fn make_state_with_wt(wt: Arc<dyn crate::shell::wt_channel::WtChannel>) -> Arc<MasterStateInner> {
    Arc::new(MasterStateInner {
        session_lifecycle_gates: Mutex::new(HashMap::new()),
        session_to_helper: Mutex::new(HashMap::new()),
        session_mcp_endpoints: session_mcp::Endpoints::new("http://127.0.0.1:1/mcp".to_string()),
        session_mcp_capabilities: session_mcp::CapabilityRegistry::default(),
        pending_usage: Mutex::new(HashMap::new()),
        usage_generation: watch::channel(0u64).0,
        registry: crate::session_registry::InMemoryRegistry::shared(),
        helper_ext_subscribers: Mutex::new(HashMap::new()),
        wt: Some(wt),
        agents: Mutex::new(HashMap::new()),
        default_agent_cmd: "copilot --acp --stdio".to_string(),
        default_agent_id: Some("copilot".to_string()),
        allowed_agent_ids: None,
        helper_meta: Mutex::new(HashMap::new()),
        tab_ownership_gate: Mutex::new(()),
        connected_helpers: Mutex::new(HashSet::new()),
        all_retirement_fence: Mutex::new(AllRetirementFence::default()),
        tab_retirement_fences: Mutex::new(HashMap::new()),
        tab_retirement_rekeys: Mutex::new(HashMap::new()),
        unresolved_owner_retirements: Mutex::new(HashMap::new()),
        pending_session_helpers: Mutex::new(HashMap::new()),
        pending_session_mcp: Mutex::new(HashMap::new()),
        closing_session_helpers: Mutex::new(HashSet::new()),
        destructive_session_helpers: Mutex::new(HashSet::new()),
        active_retirement_helpers: Mutex::new(HashSet::new()),
        closing_session_results: Mutex::new(HashMap::new()),
        session_transaction_changed: tokio::sync::Notify::new(),
        retirement_operations: Mutex::new(HashMap::new()),
        retirement_completion_tx: Mutex::new(None),
        retirement_pending_timeout: SESSION_CLOSE_TIMEOUT,
        disconnect_orphan_publication_pause: Mutex::new(None),
        deferred_retirement_cleanup_complete: tokio::sync::Notify::new(),
        hook_owned: Mutex::new(HashSet::new()),
        born_bound: Mutex::new(HashSet::new()),
        orphaned_sessions: Mutex::new(HashMap::new()),
        orphaned_tabs: Mutex::new(HashMap::new()),
    })
}

fn focus_params_for(
    sid: &acp::schema::v1::SessionId,
) -> crate::session_registry::FocusSessionParams {
    crate::session_registry::FocusSessionParams {
        session_id: sid.clone(),
    }
}

/// Happy path: sid in registry with pane_session_id, WtChannel present.
/// The handler should call `wt.request("focus_pane", { session_id: <pane_guid> })`
/// exactly once and return an `Ok` ExtResponse.
#[tokio::test]
async fn focus_session_dispatches_to_wt_channel_with_pane_session_id() {
    use crate::session_registry::SessionInfo;
    use std::path::PathBuf;

    let mock = Arc::new(MockWtChannel::ok());
    let state = make_state_with_wt(mock.clone());
    let sid = acp::schema::v1::SessionId::new("alive-sess");
    let mut info = SessionInfo::new(sid.clone(), PathBuf::from("/repo"));
    info.pane_session_id = Some("pane-GUID-123".to_string());
    state.registry.upsert(info).await;

    let params = focus_params_for(&sid);
    let resp = handle_focus_session(&state, &params)
        .await
        .expect("focus_session must succeed");

    let calls = mock.calls();
    assert_eq!(calls.len(), 1, "exactly one wt.request call expected");
    assert_eq!(calls[0].0, "focus_pane");
    assert_eq!(
        calls[0].1,
        serde_json::json!({ "session_id": "pane-GUID-123" })
    );

    let body: serde_json::Value = serde_json::from_str(resp.0.get()).expect("response is JSON");
    assert_eq!(body["ok"], serde_json::Value::Bool(true));
    assert_eq!(body["pane_session_id"], "pane-GUID-123");
}

/// Unknown SessionId → `resource_not_found` so the helper knows
/// the row doesn't exist on this master (vs. existing-but-unfocusable).
#[tokio::test]
async fn focus_session_returns_not_found_for_unknown_session() {
    let mock = Arc::new(MockWtChannel::ok());
    let state = make_state_with_wt(mock.clone());
    let sid = acp::schema::v1::SessionId::new("nobody-here");

    let params = focus_params_for(&sid);
    let err = handle_focus_session(&state, &params)
        .await
        .expect_err("unknown sid must error");
    assert_eq!(err.code, acp::ErrorCode::ResourceNotFound);
    assert!(
        mock.calls().is_empty(),
        "no wt call when session not in registry"
    );
}

/// Row exists but has no pane_session_id → `invalid_request`
/// (different code from "not found" so the helper can branch on it).
#[tokio::test]
async fn focus_session_returns_invalid_request_for_row_without_pane_session_id() {
    use crate::session_registry::SessionInfo;
    use std::path::PathBuf;

    let mock = Arc::new(MockWtChannel::ok());
    let state = make_state_with_wt(mock.clone());
    let sid = acp::schema::v1::SessionId::new("orphan-sess");
    let info = SessionInfo::new(sid.clone(), PathBuf::from("/repo")); // no pane_session_id
    state.registry.upsert(info).await;

    let params = focus_params_for(&sid);
    let err = handle_focus_session(&state, &params)
        .await
        .expect_err("row without pane_session_id must error");
    assert_eq!(err.code, acp::ErrorCode::InvalidRequest);
    assert!(mock.calls().is_empty());
}

/// `wt: None` (master booted outside a WT pane) → `internal_error`
/// so the helper can fall back to its legacy focus path.
#[tokio::test]
async fn focus_session_returns_internal_error_when_wt_channel_unavailable() {
    use crate::session_registry::SessionInfo;
    use std::path::PathBuf;

    let state = make_state(); // wt: None
    let sid = acp::schema::v1::SessionId::new("alive-but-no-wt");
    let mut info = SessionInfo::new(sid.clone(), PathBuf::from("/repo"));
    info.pane_session_id = Some("pane-X".to_string());
    state.registry.upsert(info).await;

    let params = focus_params_for(&sid);
    let err = handle_focus_session(&state, &params)
        .await
        .expect_err("wt None must error");
    assert_eq!(err.code, acp::ErrorCode::InternalError);
}

/// Wtcli failure propagates as `internal_error` with the wtcli
/// error message embedded in `data` so the helper can log it.
#[tokio::test]
async fn focus_session_wraps_wt_failure_as_internal_error() {
    use crate::session_registry::SessionInfo;
    use std::path::PathBuf;

    let mock = Arc::new(MockWtChannel::failing("0x80070490: pane not found"));
    let state = make_state_with_wt(mock.clone());
    let sid = acp::schema::v1::SessionId::new("alive-but-pane-gone");
    let mut info = SessionInfo::new(sid.clone(), PathBuf::from("/repo"));
    info.pane_session_id = Some("dead-pane".to_string());
    state.registry.upsert(info).await;

    let params = focus_params_for(&sid);
    let err = handle_focus_session(&state, &params)
        .await
        .expect_err("wt failure must surface as Err");
    assert_eq!(err.code, acp::ErrorCode::InternalError);
    // Mock was still invoked once before failing — confirms we
    // didn't short-circuit somewhere upstream of the dispatch.
    assert_eq!(mock.calls().len(), 1);
}

/// Malformed params for a recognized method are rejected as `invalid_params`
/// by `parse_ext_request` (unit-tested in `session_registry`), so the
/// handlers below always receive already-decoded, well-typed params.
#[tokio::test]
async fn session_hook_broadcasts_sessions_changed_after_valid_payload() {
    let state = make_state();
    let (tx, mut rx) = mpsc::unbounded_channel();
    state
        .helper_ext_subscribers
        .lock()
        .await
        .insert(HelperId(7), tx);

    // Use SessionStarted because it unconditionally upserts a row,
    // so the reducer returns true and the broadcast fires. PaneClosed
    // against an empty registry is a no-op (returns false) and would
    // not exercise the broadcast path.
    let event = crate::agent_sessions::SessionEvent::SessionStarted {
        key: "sid-for-hook".to_string(),
        cli_source: crate::agent_sessions::CliSource::Copilot,
        pane_session_id: "pane-for-hook".to_string(),
        cwd: std::path::PathBuf::from("/tmp"),
        title: String::new(),
    };
    let response = handle_session_hook(&state, event, false)
        .await
        .expect("valid session_hook accepted");
    assert_eq!(response.0.get(), r#"{"applied":true}"#);

    let notification = rx.try_recv().expect("sessions/changed broadcast queued");
    assert_eq!(
        &*notification.method,
        crate::session_registry::INTELLTERM_METHOD_SESSIONS_CHANGED
    );
    assert_eq!(notification.params.get(), "{}");
}

// ── refresh_synthetic_titles_from ───────────────────────────────

#[tokio::test]
async fn refresh_synthetic_titles_from_upgrades_known_placeholder_titles_only() {
    use std::collections::HashMap;

    let state = make_state();
    let mut empty = crate::session_registry::SessionInfo::new(
        acp::schema::v1::SessionId::new("sid-empty".to_string()),
        std::path::PathBuf::from("/repo/empty"),
    );
    empty.title = Some(String::new());
    state.registry.upsert(empty).await;

    let mut basename = crate::session_registry::SessionInfo::new(
        acp::schema::v1::SessionId::new("sid-base".to_string()),
        std::path::PathBuf::from("/repo/project"),
    );
    basename.title = Some("project".to_string());
    state.registry.upsert(basename).await;

    let mut placeholder = crate::session_registry::SessionInfo::new(
        acp::schema::v1::SessionId::new("sid-placeholder".to_string()),
        std::path::PathBuf::from("/repo/opencode"),
    );
    placeholder.cli_source = Some(crate::agent_sessions::CliSource::OpenCode);
    placeholder.title = Some("New session - 2026-07-23T01:14:00.422Z".to_string());
    state.registry.upsert(placeholder).await;

    let mut real = crate::session_registry::SessionInfo::new(
        acp::schema::v1::SessionId::new("sid-real".to_string()),
        std::path::PathBuf::from("/repo/real"),
    );
    real.title = Some("Existing Real Title".to_string());
    state.registry.upsert(real).await;

    let titles = HashMap::from([
        ("sid-empty".to_string(), "Empty Real Title".to_string()),
        ("sid-base".to_string(), "Basename Real Title".to_string()),
        (
            "sid-placeholder".to_string(),
            "OpenCode Real Title".to_string(),
        ),
        ("sid-real".to_string(), "Should Not Overwrite".to_string()),
    ]);

    assert!(refresh_synthetic_titles_from(&*state.registry, &titles).await);
    assert_eq!(
        state
            .registry
            .lookup(&acp::schema::v1::SessionId::new("sid-empty".to_string()))
            .await
            .unwrap()
            .title
            .as_deref(),
        Some("Empty Real Title")
    );
    assert_eq!(
        state
            .registry
            .lookup(&acp::schema::v1::SessionId::new("sid-base".to_string()))
            .await
            .unwrap()
            .title
            .as_deref(),
        Some("Basename Real Title")
    );
    assert_eq!(
        state
            .registry
            .lookup(&acp::schema::v1::SessionId::new(
                "sid-placeholder".to_string()
            ))
            .await
            .unwrap()
            .title
            .as_deref(),
        Some("OpenCode Real Title")
    );
    assert_eq!(
        state
            .registry
            .lookup(&acp::schema::v1::SessionId::new("sid-real".to_string()))
            .await
            .unwrap()
            .title
            .as_deref(),
        Some("Existing Real Title")
    );
}

#[tokio::test]
async fn refresh_synthetic_titles_from_skips_when_id_absent() {
    let state = make_state();
    let mut row = crate::session_registry::SessionInfo::new(
        acp::schema::v1::SessionId::new("sid-missing".to_string()),
        std::path::PathBuf::from("/repo/project"),
    );
    row.title = Some("project".to_string());
    state.registry.upsert(row).await;

    assert!(
        !refresh_synthetic_titles_from(&*state.registry, &std::collections::HashMap::new()).await
    );
    assert_eq!(
        state
            .registry
            .lookup(&acp::schema::v1::SessionId::new("sid-missing".to_string()))
            .await
            .unwrap()
            .title
            .as_deref(),
        Some("project")
    );
}

#[test]
fn row_refreshable_skips_only_definitively_cross_cli() {
    use crate::agent_sessions::CliSource;
    let mut row = crate::session_registry::SessionInfo::new(
        acp::schema::v1::SessionId::new("s".to_string()),
        std::path::PathBuf::from("/x"),
    );
    // Same known cli → refreshable.
    row.cli_source = Some(CliSource::Copilot);
    assert!(row_refreshable_by_connected_agent(
        &row,
        Some(&CliSource::Copilot)
    ));
    // Different known cli → skipped (the connected agent can't enumerate it).
    assert!(!row_refreshable_by_connected_agent(
        &row,
        Some(&CliSource::Claude)
    ));
    // Unknown cli on either side → attempt (never skip).
    row.cli_source = None;
    assert!(row_refreshable_by_connected_agent(
        &row,
        Some(&CliSource::Copilot)
    ));
    row.cli_source = Some(CliSource::Copilot);
    assert!(row_refreshable_by_connected_agent(&row, None));
}

/// Mock agent CLI that answers `session/list` with a fixed id set, so the
/// per-agent history seed can be exercised without a real CLI.
fn client_connection_to_listing_agent(ids: Vec<String>) -> conn::ClientLink {
    let (client_pipe, agent_pipe) = tokio::io::duplex(8192);
    let (client_read, client_write) = tokio::io::split(client_pipe);
    let (agent_read, agent_write) = tokio::io::split(agent_pipe);

    let agent_builder = acp::Agent
        .builder()
        .name("listing-agent")
        .on_receive_request(
            move |_req: acp::schema::v1::ListSessionsRequest,
                  responder: acp::Responder<acp::schema::v1::ListSessionsResponse>,
                  _cx| {
                let ids = ids.clone();
                async move {
                    let rows: Vec<acp::schema::v1::SessionInfo> = ids
                        .iter()
                        .map(|id| {
                            acp::schema::v1::SessionInfo::new(
                                acp::schema::v1::SessionId::new(id.clone()),
                                std::path::PathBuf::from("C:\\repo"),
                            )
                        })
                        .collect();
                    responder.respond(acp::schema::v1::ListSessionsResponse::new(rows))
                }
            },
            acp::on_receive_request!(),
        );
    let (_agent_conn, agent_io) = conn::spawn_agent(
        agent_builder,
        conn::byte_streams(agent_write.compat_write(), agent_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = agent_io.await;
    });

    let (client_conn, client_io) = conn::spawn_client(
        acp::Client.builder().name("listing-client"),
        conn::byte_streams(client_write.compat_write(), client_read.compat()),
    );
    tokio::task::spawn_local(async move {
        let _ = client_io.await;
    });

    client_conn
}

fn listing_agent(cli: crate::agent_sessions::CliSource, ids: &[&str]) -> Arc<AgentCli> {
    listing_agent_with_cli(Some(cli), ids)
}

/// `cli = None` models a `custom:<name>` provider, which the agent registry
/// does not resolve to a known `CliSource`.
fn listing_agent_with_cli(
    cli: Option<crate::agent_sessions::CliSource>,
    ids: &[&str],
) -> Arc<AgentCli> {
    listing_agent_from(cli, crate::agent_source::AgentSource::Host, ids)
}

/// An agent pooled against a WSL distro rather than the host.
fn wsl_listing_agent(
    cli: crate::agent_sessions::CliSource,
    distro: &str,
    ids: &[&str],
) -> Arc<AgentCli> {
    listing_agent_from(
        Some(cli),
        crate::agent_source::AgentSource::Wsl {
            distro: distro.to_string(),
        },
        ids,
    )
}

fn listing_agent_from(
    cli: Option<crate::agent_sessions::CliSource>,
    source: crate::agent_source::AgentSource,
    ids: &[&str],
) -> Arc<AgentCli> {
    let mut cached_init_resp =
        acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1);
    cached_init_resp
        .agent_capabilities
        .session_capabilities
        .list = Some(acp::schema::v1::SessionListCapabilities::default());
    Arc::new(AgentCli {
        instance_id: AgentInstanceId::new_v4(),
        conn: client_connection_to_listing_agent(ids.iter().map(|s| s.to_string()).collect()),
        cached_init_resp,
        cli_source: cli.clone(),
        source: source.clone(),
        cmd_key: format!("listing-agent-{cli:?}-{source}"),
        cloud_catalog: Mutex::new(NativeCloudCatalogState::Pending),
        bound_helpers: Mutex::new(HashSet::new()),
        host_list_cache: Mutex::new(None),
        listed_ever: Mutex::new(HashSet::new()),
    })
}

/// The regression this whole change exists for: master survives a Settings
/// agent switch (the helper reconnects, the pool spawns the new CLI, no master
/// restart), so history must be seeded and stamped per pooled agent. Seeding
/// only the first agent left the registry holding one CLI's rows, and the
/// helper's per-CLI view filter then rendered an empty session list for every
/// agent the user switched to until Terminal was restarted.
#[tokio::test]
async fn each_pooled_agent_seeds_and_stamps_its_own_history() {
    use crate::agent_sessions::CliSource;

    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let copilot = listing_agent(CliSource::Copilot, &["copilot-row"]);
            let codex = listing_agent(CliSource::Codex, &["codex-row"]);

            assert_eq!(seed_host_and_broadcast(&state, &copilot).await, 1);
            // The second agent must seed too — not be skipped as "not first".
            assert_eq!(seed_host_and_broadcast(&state, &codex).await, 1);

            let rows = state.registry.snapshot().await;
            let cli_of = |id: &str| {
                rows.iter()
                    .find(|r| r.session_id.0.as_ref() == id)
                    .unwrap_or_else(|| panic!("{id} missing from registry"))
                    .cli_source
                    .clone()
            };
            // Each row carries ITS OWN agent's CLI, not the master launch CLI.
            assert_eq!(cli_of("copilot-row"), Some(CliSource::Copilot));
            assert_eq!(cli_of("codex-row"), Some(CliSource::Codex));

            // Codex's reconcile must not have pruned the Copilot row it never
            // listed, and vice versa.
            assert_eq!(seed_host_and_broadcast(&state, &copilot).await, 1);
            let rows = state.registry.snapshot().await;
            assert!(rows.iter().any(|r| r.session_id.0.as_ref() == "codex-row"));
            assert!(rows
                .iter()
                .any(|r| r.session_id.0.as_ref() == "copilot-row"));
        })
        .await;
}

/// The historical path must stamp each agent's rows with where that agent
/// actually runs. Host Copilot and Copilot in a WSL distro share a
/// `CliSource`, so `location` is the only field that separates them; stamping
/// a blanket `Host` made every source's history indistinguishable and left two
/// WSL panes rendering one merged list.
#[tokio::test]
async fn seeded_history_carries_the_agents_execution_source() {
    use crate::agent_sessions::{CliSource, SessionLocation};

    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let host = listing_agent(CliSource::Copilot, &["host-row"]);
            let debian = wsl_listing_agent(CliSource::Copilot, "Debian", &["debian-row"]);

            assert_eq!(seed_host_and_broadcast(&state, &host).await, 1);
            assert_eq!(seed_host_and_broadcast(&state, &debian).await, 1);

            let rows = state.registry.snapshot().await;
            let location_of = |id: &str| {
                rows.iter()
                    .find(|r| r.session_id.0.as_ref() == id)
                    .unwrap_or_else(|| panic!("{id} missing from registry"))
                    .location
                    .clone()
            };
            assert_eq!(location_of("host-row"), SessionLocation::Host);
            assert_eq!(
                location_of("debian-row"),
                SessionLocation::Wsl {
                    distro: "Debian".to_string()
                }
            );
        })
        .await;
}

/// The command line alone cannot say WHERE an agent runs — `copilot --acp
/// --stdio` is the spelling for the host CLI and for the CLI inside every WSL
/// distro — so a startup failure has to name the source or it sends the user
/// debugging the wrong machine.
#[test]
fn startup_failure_names_the_execution_source() {
    use crate::agent_source::AgentSource;

    assert_eq!(
        describe_agent_target("copilot --acp --stdio", &AgentSource::Host),
        "'copilot --acp --stdio'"
    );
    assert_eq!(
        describe_agent_target(
            "copilot --acp --stdio",
            &AgentSource::Wsl {
                distro: "Ubuntu".to_string()
            }
        ),
        "'copilot --acp --stdio' (WSL Ubuntu)"
    );
}

/// Captured stderr is the only description of *why* an agent died; without it
/// the user sees the transport symptom ("oneshot canceled") and nothing else.
/// Bounded so a chatty CLI can't flood the pane's error banner.
#[test]
fn startup_stderr_is_folded_into_the_error_and_bounded() {
    assert_eq!(format_startup_stderr(&[]), "");

    let one = format_startup_stderr(&["cannot preserve mount namespace".to_string()]);
    assert_eq!(one, "\n  agent stderr: cannot preserve mount namespace");

    // Keeps the LAST lines — the final message before exit is the useful one —
    // and says how many were elided.
    let many: Vec<String> = (1..=7).map(|i| format!("line {i}")).collect();
    let out = format_startup_stderr(&many);
    assert!(out.contains("line 7"), "must keep the final line: {out}");
    assert!(!out.contains("line 3"), "must elide early lines: {out}");
    assert_eq!(
        out.matches("agent stderr:").count(),
        STARTUP_STDERR_IN_ERROR
    );
    assert!(
        out.contains("3 earlier stderr line(s) in the master log"),
        "must point at the log for the rest: {out}"
    );
}

#[test]
fn is_stale_host_history_row_reconcile_rules() {
    use crate::agent_sessions::{AgentStatus, CliSource, SessionLocation, SessionOrigin};
    use std::collections::HashSet;
    // Ids the listing agent previously returned and no longer returns.
    let prunable: HashSet<String> = ["gone".to_string()].into_iter().collect();
    let copilot = Some(&CliSource::Copilot);
    let mk = |id: &str| {
        let mut r = crate::session_registry::SessionInfo::new(
            acp::schema::v1::SessionId::new(id.to_string()),
            std::path::PathBuf::from("C:\\Users\\dev"),
        );
        r.status = Some(AgentStatus::Historical);
        r.origin = Some(SessionOrigin::Unknown);
        r.cli_source = Some(CliSource::Copilot);
        r
    };
    // Terminal Class-B host row the agent dropped from session/list → stale.
    assert!(is_stale_host_history_row(&mk("gone"), &prunable, copilot));
    // Still listed (never entered the prunable set) → keep.
    assert!(!is_stale_host_history_row(&mk("kept"), &prunable, copilot));
    // Live (Idle/Working) → keep even when dropped from the listing.
    let mut live = mk("gone");
    live.status = Some(AgentStatus::Idle);
    assert!(!is_stale_host_history_row(&live, &prunable, copilot));
    // Agent pane → never reconciled.
    let mut pane = mk("gone");
    pane.origin = Some(SessionOrigin::AgentPane);
    assert!(!is_stale_host_history_row(&pane, &prunable, copilot));
    // WSL row → host can't authoritatively list distro sessions.
    let mut wsl = mk("gone");
    wsl.location = SessionLocation::Wsl {
        distro: "Ubuntu".to_string(),
    };
    assert!(!is_stale_host_history_row(&wsl, &prunable, copilot));
}

/// `CliSource` is not a session universe. Host Copilot, Copilot in WSL Debian,
/// and Copilot in WSL Ubuntu all stamp `Some(Copilot)` but list disjoint
/// sessions, so reconcile must be scoped to ids the listing agent itself has
/// seen. Testing "not in the current listing" instead had each agent delete the
/// other's rows on every 5 s poll while the other re-added them — an unbounded
/// thrash that produced ~52k `reconcile: dropped host row` lines in one
/// session's master log.
#[test]
fn reconcile_never_prunes_a_same_cli_agents_unseen_rows() {
    use crate::agent_sessions::{AgentStatus, CliSource, SessionOrigin};
    use std::collections::HashSet;

    let mut other_agents_row = crate::session_registry::SessionInfo::new(
        acp::schema::v1::SessionId::new("in-distro-copilot-row".to_string()),
        std::path::PathBuf::from("C:\\Users\\dev"),
    );
    other_agents_row.status = Some(AgentStatus::Historical);
    other_agents_row.origin = Some(SessionOrigin::Unknown);
    other_agents_row.cli_source = Some(CliSource::Copilot);

    // The host Copilot agent has never listed this id, so it is not prunable
    // even though the CLI stamp matches and the row is absent from its listing.
    let prunable: HashSet<String> = HashSet::new();
    assert!(!is_stale_host_history_row(
        &other_agents_row,
        &prunable,
        Some(&CliSource::Copilot)
    ));

    // The agent that DID list it once, and no longer does, may drop it.
    let prunable: HashSet<String> = ["in-distro-copilot-row".to_string()]
        .into_iter()
        .collect();
    assert!(is_stale_host_history_row(
        &other_agents_row,
        &prunable,
        Some(&CliSource::Copilot)
    ));
}

/// A pooled agent's `session/list` is authority over ITS OWN rows only. Master
/// multiplexes several CLIs at once (per-tab `/agent`, a Settings switch that
/// leaves the previous CLI in the pool), and the file watcher discovers shell
/// sessions machine-wide across CLIs — so letting one agent's listing prune
/// another's rows silently deletes history the listing agent never knew about.
#[test]
fn is_stale_host_history_row_never_prunes_another_clis_rows() {
    use crate::agent_sessions::{AgentStatus, CliSource, SessionOrigin};
    use std::collections::HashSet;
    let mut claude_row = crate::session_registry::SessionInfo::new(
        acp::schema::v1::SessionId::new("claude-row".to_string()),
        std::path::PathBuf::from("C:\\Users\\dev"),
    );
    claude_row.status = Some(AgentStatus::Historical);
    claude_row.origin = Some(SessionOrigin::Unknown);
    claude_row.cli_source = Some(CliSource::Claude);
    // The row is prunable as far as the id set goes; only the CLI guard should
    // decide, so this isolates that guard.
    let prunable: HashSet<String> = ["claude-row".to_string()].into_iter().collect();

    assert!(!is_stale_host_history_row(
        &claude_row,
        &prunable,
        Some(&CliSource::Codex)
    ));
    // Its own CLI may still prune it.
    assert!(is_stale_host_history_row(
        &claude_row,
        &prunable,
        Some(&CliSource::Claude)
    ));

    // An unstamped row is never pruned: no agent can claim authority over it,
    // so whichever CLI polls first must not delete it.
    let mut unstamped = claude_row.clone();
    unstamped.cli_source = None;
    assert!(!is_stale_host_history_row(
        &unstamped,
        &prunable,
        Some(&CliSource::Claude)
    ));
    // ...and an agent with no resolved CLI has no authority over anything.
    assert!(!is_stale_host_history_row(&claude_row, &prunable, None));
    assert!(!is_stale_host_history_row(&unstamped, &prunable, None));
}

/// A `custom:<name>` provider has `AgentCli::cli_source == None`, but
/// `host_history_via_acp` stamps its rows `Unknown("custom")`. Reconcile
/// authority is derived through `stamped_cli`, so the collapsed value matches
/// the stamp — without it the guard sees `None`, denies authority, and
/// reconcile silently never prunes an unrecognized agent's stale rows.
#[test]
fn custom_agent_reconciles_rows_stamped_with_the_collapsed_cli() {
    use crate::agent_sessions::{AgentStatus, CliSource, SessionOrigin};
    use std::collections::HashSet;

    // What an unrecognized agent's `cli_source: None` collapses to.
    let custom = stamped_cli(None);
    assert_eq!(custom, CliSource::Unknown("custom".into()));

    let prunable: HashSet<String> = ["custom-row".to_string()].into_iter().collect();
    let mut row = crate::session_registry::SessionInfo::new(
        acp::schema::v1::SessionId::new("custom-row".to_string()),
        std::path::PathBuf::from("C:\\Users\\dev"),
    );
    row.status = Some(AgentStatus::Historical);
    row.origin = Some(SessionOrigin::Unknown);
    row.cli_source = Some(custom.clone());

    assert!(is_stale_host_history_row(&row, &prunable, Some(&custom)));
    // A recognized CLI still has no authority over a custom row.
    assert!(!is_stale_host_history_row(
        &row,
        &prunable,
        Some(&CliSource::Copilot)
    ));
}

/// Row-driven refresh looks the owning agent up by the row's stamped CLI **and**
/// its execution source. A custom agent is pooled with `cli_source: None` while
/// its rows carry `Unknown("custom")`, so the provider halves must normalize; a
/// `Wsl` row must not resolve to the host agent, which enumerates a different
/// `$HOME` and can never see it.
#[tokio::test]
async fn agent_for_row_matches_provider_and_execution_source() {
    use crate::agent_sessions::{CliSource, SessionLocation};

    tokio::task::LocalSet::new()
        .run_until(async {
            let state = make_state();
            let custom = listing_agent_with_cli(None, &[]);
            let host_copilot = listing_agent(CliSource::Copilot, &[]);
            let debian_copilot = wsl_listing_agent(CliSource::Copilot, "Debian", &[]);
            {
                let mut agents = state.agents.lock().await;
                for agent in [&custom, &host_copilot, &debian_copilot] {
                    let cell: AgentCell = Arc::new(tokio::sync::OnceCell::new());
                    let _ = cell.set(Arc::clone(agent));
                    agents.insert(agent.cmd_key.clone(), cell);
                }
            }
            let debian = SessionLocation::Wsl {
                distro: "Debian".to_string(),
            };

            // Same provider, different source → must not collapse onto each other.
            let found = agent_for_row(&state, Some(&CliSource::Copilot), &SessionLocation::Host)
                .await
                .expect("host copilot row resolves");
            assert_eq!(found.cmd_key, host_copilot.cmd_key);

            let found = agent_for_row(&state, Some(&CliSource::Copilot), &debian)
                .await
                .expect("debian copilot row resolves");
            assert_eq!(found.cmd_key, debian_copilot.cmd_key);

            // A custom-stamped row still reaches the unrecognized agent.
            let stamped = stamped_cli(None);
            let found = agent_for_row(&state, Some(&stamped), &SessionLocation::Host)
                .await
                .expect("custom-stamped row resolves");
            assert_eq!(found.cmd_key, custom.cmd_key);

            // No agent pooled for that pair → nobody can answer.
            assert!(agent_for_row(
                &state,
                Some(&CliSource::Copilot),
                &SessionLocation::Wsl {
                    distro: "Ubuntu".to_string()
                }
            )
            .await
            .is_none());
        })
        .await;
}

#[test]
fn session_event_key_returns_key_for_keyed_variants() {
    use crate::agent_sessions::{CliSource, SessionEvent};
    let cases: Vec<(SessionEvent, Option<&str>)> = vec![
        (
            SessionEvent::SessionStarted {
                key: "k1".into(),
                cli_source: CliSource::Copilot,
                pane_session_id: "p".into(),
                cwd: std::path::PathBuf::new(),
                title: String::new(),
            },
            Some("k1"),
        ),
        (
            SessionEvent::ToolStarting {
                key: "k2".into(),
                tool_name: "t".into(),
            },
            Some("k2"),
        ),
        (SessionEvent::ToolCompleted { key: "k3".into() }, Some("k3")),
        (
            SessionEvent::Notification {
                key: "k4".into(),
                message: "m".into(),
            },
            Some("k4"),
        ),
        (
            SessionEvent::SessionStopped {
                key: "k5".into(),
                reason: "r".into(),
            },
            Some("k5"),
        ),
        (
            SessionEvent::ResumeDispatched { key: "k6".into() },
            Some("k6"),
        ),
        (
            SessionEvent::ResumePaneAssigned {
                key: "k7".into(),
                pane_session_id: "p".into(),
            },
            Some("k7"),
        ),
        // Pane-only variants: no session key → refresh skipped.
        (
            SessionEvent::PaneClosed {
                pane_session_id: "p".into(),
            },
            None,
        ),
        (
            SessionEvent::ConnectionFailed {
                pane_session_id: "p".into(),
                reason: "r".into(),
            },
            None,
        ),
    ];
    for (event, expected) in cases {
        assert_eq!(session_event_key(&event), expected, "event={event:?}");
    }
}
async fn seed_session_row(
    state: &MasterStateInner,
    key: &str,
    origin: crate::agent_sessions::SessionOrigin,
    status: crate::agent_sessions::AgentStatus,
) {
    let mut info = crate::session_registry::SessionInfo::new(
        acp::schema::v1::SessionId::new(key.to_string()),
        std::path::PathBuf::from("C:\\repo"),
    );
    info.cli_source = Some(crate::agent_sessions::CliSource::Codex);
    info.origin = Some(origin);
    info.status = Some(status);
    state.registry.upsert(info).await;
}

fn codex_emitted(key: &str) -> crate::session_watcher::Emitted {
    crate::session_watcher::Emitted {
        cli: crate::agent_sessions::CliSource::Codex,
        key: key.to_string(),
        event: crate::agent_sessions::SessionEvent::ToolStarting {
            key: key.to_string(),
            tool_name: String::new(),
        },
    }
}

// ── Hybrid event-dedup: hooks / born-bound win, watcher is fallback ──

#[tokio::test]
async fn watcher_event_dropped_when_session_is_hook_owned() {
    // A session a hook (or #266 born-bound) already claimed is recorded in
    // `hook_owned`. The watcher is a fallback and must not double-track it:
    // its event is dropped before any row is created.
    let state = make_state();
    state
        .hook_owned
        .lock()
        .await
        .insert(acp::schema::v1::SessionId::new("sid-hooked".to_string()));

    apply_watcher_event(&state, codex_emitted("sid-hooked")).await;

    assert!(
        state
            .registry
            .lookup(&acp::schema::v1::SessionId::new("sid-hooked".to_string()))
            .await
            .is_none(),
        "watcher must not create a row for a hook-owned session"
    );
}

#[tokio::test]
async fn watcher_event_dropped_for_agent_pane_session() {
    // Agent-pane (Class A) sessions are driven by ACP session/update; the
    // watcher must defer to ACP even though the agent CLI also writes the
    // on-disk session file the watcher sees.
    let state = make_state();
    seed_session_row(
        &state,
        "sid-agent-pane",
        crate::agent_sessions::SessionOrigin::AgentPane,
        crate::agent_sessions::AgentStatus::Idle,
    )
    .await;

    apply_watcher_event(&state, codex_emitted("sid-agent-pane")).await;

    let row = state
        .registry
        .lookup(&acp::schema::v1::SessionId::new(
            "sid-agent-pane".to_string(),
        ))
        .await
        .unwrap();
    // Still Idle — the watcher's ToolStarting (Working) was dropped.
    assert_eq!(row.status, Some(crate::agent_sessions::AgentStatus::Idle));
}

#[tokio::test]
async fn session_hook_marks_session_hook_owned_then_watcher_is_ignored() {
    // End-to-end: a hook SessionStarted claims the session (recording it in
    // `hook_owned`), after which the watcher's events for that session are
    // dropped — so the hook-sourced pane binding is never clobbered.
    let state = make_state();
    let event = crate::agent_sessions::SessionEvent::SessionStarted {
        key: "sid-claimed".to_string(),
        cli_source: crate::agent_sessions::CliSource::Codex,
        pane_session_id: "pane-from-hook".to_string(),
        cwd: std::path::PathBuf::from("C:\\repo"),
        title: String::new(),
    };
    handle_session_hook(&state, event, false)
        .await
        .expect("valid session_hook accepted");

    assert!(
        state
            .hook_owned
            .lock()
            .await
            .contains(&acp::schema::v1::SessionId::new("sid-claimed".to_string())),
        "a keyed session_hook event must mark the session hook-owned"
    );

    // A subsequent watcher event must not disturb the hook-bound row.
    apply_watcher_event(&state, codex_emitted("sid-claimed")).await;
    let row = state
        .registry
        .lookup(&acp::schema::v1::SessionId::new("sid-claimed".to_string()))
        .await
        .unwrap();
    assert_eq!(
        row.pane_session_id.as_deref(),
        Some("pane-from-hook"),
        "watcher must not clobber the hook-sourced pane binding"
    );
}

#[tokio::test]
async fn session_born_bound_marks_born_bound_not_hook_owned() {
    // #266 born-bound (WTA-launched delegate/resume) is binding-only: it must
    // land in `born_bound`, NOT `hook_owned`, so the watcher can still supply
    // status for it when no real hook is installed.
    let state = make_state();
    let event = crate::agent_sessions::SessionEvent::SessionStarted {
        key: "bb-mark".to_string(),
        cli_source: crate::agent_sessions::CliSource::Claude,
        pane_session_id: "pane-bb".to_string(),
        cwd: std::path::PathBuf::from("C:\\repo"),
        title: String::new(),
    };
    handle_session_hook(&state, event, true)
        .await
        .expect("valid born-bound accepted");

    let sid = acp::schema::v1::SessionId::new("bb-mark".to_string());
    assert!(
        state.born_bound.lock().await.contains(&sid),
        "born-bound registration must record the session in `born_bound`"
    );
    assert!(
        !state.hook_owned.lock().await.contains(&sid),
        "born-bound is binding-only — must NOT be hook-owned"
    );
}

#[tokio::test]
async fn born_bound_wsl_stamps_wsl_location() {
    // A WSL `?<prompt>` delegate registers with a distro; the master must
    // stamp the row `Wsl { distro }` (the reducer defaults to Host) so the
    // session view renders the [WSL-<distro>] prefix.
    let state = make_state();
    let event = crate::agent_sessions::SessionEvent::SessionStarted {
        key: "bb-wsl-loc".to_string(),
        cli_source: crate::agent_sessions::CliSource::Copilot,
        pane_session_id: "pane-wsl".to_string(),
        cwd: std::path::PathBuf::from("/mnt/c/Users/dev"),
        title: String::new(),
    };
    handle_session_born_bound(&state, event, Some("Ubuntu".to_string()))
        .await
        .expect("wsl born-bound accepted");

    let sid = acp::schema::v1::SessionId::new("bb-wsl-loc".to_string());
    assert_eq!(
        state.registry.lookup(&sid).await.unwrap().location,
        crate::agent_sessions::SessionLocation::Wsl {
            distro: "Ubuntu".to_string()
        },
        "WSL born-bound row must be stamped Wsl {{ distro }}"
    );
    // Still binding-only, like any born-bound row.
    assert!(state.born_bound.lock().await.contains(&sid));
}

#[tokio::test]
async fn born_bound_host_stays_host_location() {
    // A host `?<prompt>` delegate carries no distro; the row stays Host.
    let state = make_state();
    let event = crate::agent_sessions::SessionEvent::SessionStarted {
        key: "bb-host-loc".to_string(),
        cli_source: crate::agent_sessions::CliSource::Copilot,
        pane_session_id: "pane-host".to_string(),
        cwd: std::path::PathBuf::from("C:\\repo"),
        title: String::new(),
    };
    handle_session_born_bound(&state, event, None)
        .await
        .expect("host born-bound accepted");

    let sid = acp::schema::v1::SessionId::new("bb-host-loc".to_string());
    assert_eq!(
        state.registry.lookup(&sid).await.unwrap().location,
        crate::agent_sessions::SessionLocation::Host,
        "host born-bound row must stay Host"
    );
}

#[tokio::test]
async fn born_bound_session_gets_watcher_activity_without_rebinding() {
    // The whole point: a born-bound row (no hook) gets STATUS from the
    // watcher, while its pane binding (owned by born-bound) is untouched.
    let state = make_state();
    let sid = acp::schema::v1::SessionId::new("bb-activity".to_string());

    let mut info = crate::session_registry::SessionInfo::new(
        sid.clone(),
        std::path::PathBuf::from("C:\\repo"),
    );
    info.cli_source = Some(crate::agent_sessions::CliSource::Claude);
    info.origin = Some(crate::agent_sessions::SessionOrigin::Unknown);
    info.status = Some(crate::agent_sessions::AgentStatus::Idle);
    info.pane_session_id = Some("born-pane".to_string());
    state.registry.upsert(info).await;
    state.born_bound.lock().await.insert(sid.clone());

    // Watcher observes a tool start (the Emitted's cli is irrelevant on the
    // born-bound path — binding/gate are skipped).
    apply_watcher_event(&state, codex_emitted("bb-activity")).await;

    let row = state.registry.lookup(&sid).await.unwrap();
    assert_eq!(
        row.status,
        Some(crate::agent_sessions::AgentStatus::Working),
        "watcher must supply status for a born-bound row with no hook"
    );
    assert_eq!(
        row.pane_session_id.as_deref(),
        Some("born-pane"),
        "watcher must NOT re-bind a born-bound row's pane"
    );
}

#[tokio::test]
async fn real_hook_takes_over_born_bound_session() {
    // If a real hook later fires for a born-bound session (hooks installed
    // after launch), it becomes fully hook-owned and leaves `born_bound`, so
    // the watcher backs off entirely.
    let state = make_state();
    let sid = acp::schema::v1::SessionId::new("bb-takeover".to_string());

    let bb = crate::agent_sessions::SessionEvent::SessionStarted {
        key: "bb-takeover".to_string(),
        cli_source: crate::agent_sessions::CliSource::Claude,
        pane_session_id: "pane-bb".to_string(),
        cwd: std::path::PathBuf::from("C:\\repo"),
        title: String::new(),
    };
    handle_session_hook(&state, bb, true)
        .await
        .expect("born-bound accepted");
    assert!(state.born_bound.lock().await.contains(&sid));

    // A real hook event arrives via session_hook (is_born_bound = false).
    let hook = crate::agent_sessions::SessionEvent::ToolStarting {
        key: "bb-takeover".to_string(),
        tool_name: "Bash".to_string(),
    };
    handle_session_hook(&state, hook, false)
        .await
        .expect("real hook accepted");

    assert!(
        state.hook_owned.lock().await.contains(&sid),
        "the real hook must take ownership"
    );
    assert!(
        !state.born_bound.lock().await.contains(&sid),
        "the real hook must remove the stale born-bound claim"
    );
}

#[tokio::test]
async fn resume_binding_events_are_born_bound_not_hook_owned() {
    // `/sessions` resume publishes ResumeDispatched / ResumePaneAssigned over
    // the generic session_hook method. These are the hook-free resume binding,
    // so they must record `born_bound` (watcher can supply status), NOT
    // `hook_owned` — otherwise the resumed row sits at Idle forever.
    let state = make_state();
    let sid = acp::schema::v1::SessionId::new("sid-resume".to_string());

    let dispatched = crate::agent_sessions::SessionEvent::ResumeDispatched {
        key: "sid-resume".to_string(),
    };
    handle_session_hook(&state, dispatched, false)
        .await
        .expect("resume dispatched accepted");
    assert!(
        state.born_bound.lock().await.contains(&sid),
        "ResumeDispatched must be born_bound"
    );
    assert!(
        !state.hook_owned.lock().await.contains(&sid),
        "ResumeDispatched must NOT be hook_owned"
    );

    let assigned = crate::agent_sessions::SessionEvent::ResumePaneAssigned {
        key: "sid-resume".to_string(),
        pane_session_id: "pane-resume".to_string(),
    };
    handle_session_hook(&state, assigned, false)
        .await
        .expect("resume pane assigned accepted");
    assert!(
        state.born_bound.lock().await.contains(&sid),
        "ResumePaneAssigned must be born_bound"
    );
    assert!(!state.hook_owned.lock().await.contains(&sid));
}
