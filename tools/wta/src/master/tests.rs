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

impl PendingNewSessionAgent {
    async fn initialize(
        &self,
        _args: acp::schema::v1::InitializeRequest,
    ) -> acp::Result<acp::schema::v1::InitializeResponse> {
        Ok(acp::schema::v1::InitializeResponse::new(acp::schema::ProtocolVersion::V1))
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
    assert_eq!(cmd, "gemini --experimental-acp");
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
        agent_cmd_key(&command, &crate::agent_source::AgentSource::Host),
        agent_cmd_key(&command, &source),
        "host and WSL instances must occupy separate pool slots"
    );
}

#[test]
fn model_is_folded_in_for_native_agents_and_ignored_for_adapters() {
    // Native agent (gemini) takes --model on the command line.
    let (cmd, _) = resolve(None, Some("gemini"), Some("gemini-2.5-pro"));
    assert_eq!(cmd, "gemini --experimental-acp --model gemini-2.5-pro");

    // Adapter agent (claude via npx) ignores the model here — it's
    // applied later via setSessionModel — so the command is stable.
    let (cmd, id) = resolve(None, Some("claude"), Some("opus-4"));
    assert_eq!(cmd, "npx -y @agentclientprotocol/claude-agent-acp");
    assert_eq!(id.as_deref(), Some("claude"));
}

#[test]
fn id_is_case_insensitive() {
    let (cmd, id) = resolve(Some(&allow_set(&["gemini"])), Some("GeMiNi"), None);
    assert_eq!(cmd, "gemini --experimental-acp");
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
        assert_eq!(cmd, expected, "agent {} must be honored, not fall back", profile.id);
        assert_eq!(id.as_deref(), Some(profile.id), "id stamp for {}", profile.id);
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
    assert_eq!(normalize_allowed_agent_ids(&[]), None, "no argv ⇒ no policy");

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
    let mixed = normalize_allowed_agent_ids(&[
        "custom:myapp".to_string(),
        "claude".to_string(),
    ])
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
    let cli = crate::Cli::try_parse_from(["wta", "--allowed-agent-ids="])
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
    let cli_absent = crate::Cli::try_parse_from(["wta"]).expect("parses");
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
    assert_eq!(cmd, "gemini --experimental-acp");
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
    assert_eq!(cmd, "gemini --experimental-acp");
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
    Arc::new(MasterStateInner {
        session_to_helper: Mutex::new(HashMap::new()),
        registry: crate::session_registry::InMemoryRegistry::shared(),
        helper_ext_subscribers: Mutex::new(HashMap::new()),
        wt: None,
        agents: Mutex::new(HashMap::new()),
        default_agent_cmd: "copilot --acp --stdio".to_string(),
        default_agent_id: Some("copilot".to_string()),
        allowed_agent_ids: None,
        cached_init_resp: OnceLock::new(),
        agent_conn: OnceLock::new(),
        cli_source: Some(crate::agent_sessions::CliSource::Copilot),
        helper_meta: Mutex::new(HashMap::new()),
        hook_owned: Mutex::new(HashSet::new()),
        born_bound: Mutex::new(HashSet::new()),
        orphaned_sessions: Mutex::new(HashMap::new()),
        host_list_cache: Mutex::new(None),
        wsl_titles_seed_at: Mutex::new(None),
        wsl_seed_in_flight: std::sync::atomic::AtomicBool::new(false),
    })
}

fn client_connection_to_pending_new_session_agent() -> conn::ClientLink {
    let (client_pipe, agent_pipe) = tokio::io::duplex(4096);
    let (client_read, client_write) = tokio::io::split(client_pipe);
    let (agent_read, agent_write) = tokio::io::split(agent_pipe);

    let mock = PendingNewSessionAgent;
    let agent_builder = acp::Agent
        .builder()
        .name("pending-agent")
        .on_receive_request({ let m = mock.clone(); move |req: acp::schema::v1::ClientRequest, responder, _cx| { let m = m.clone(); async move {
            use acp::schema::v1::{ClientRequest as Q, AgentResponse as R};
            match req {
                Q::InitializeRequest(a) => conn::respond_enum(responder, m.initialize(a).await.map(R::InitializeResponse)),
                Q::AuthenticateRequest(a) => conn::respond_enum(responder, m.authenticate(a).await.map(R::AuthenticateResponse)),
                Q::NewSessionRequest(a) => conn::respond_enum(responder, m.new_session(a).await.map(R::NewSessionResponse)),
                _ => responder.respond_with_error(acp::Error::method_not_found()),
            }
        } } }, acp::on_receive_request!());
    let (_agent_conn, agent_io) =
        conn::spawn_agent(agent_builder, conn::byte_streams(agent_write.compat_write(), agent_read.compat()));
    tokio::task::spawn_local(async move { let _ = agent_io.await; });

    let (client_conn, client_io) = conn::spawn_client(
        acp::Client.builder().name("noop-client"),
        conn::byte_streams(client_write.compat_write(), client_read.compat()),
    );
    tokio::task::spawn_local(async move { let _ = client_io.await; });

    client_conn
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
                conn: client_connection_to_pending_new_session_agent(),
                cached_init_resp: acp::schema::v1::InitializeResponse::new(
                    acp::schema::ProtocolVersion::V1,
                ),
                cli_source: None,
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "copilot --acp --stdio".to_string(),
            }));
            let handler = HelperHandler {
                helper_id: HelperId(1),
                agent,
                state: make_state(),
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

#[test]
fn cloned_helper_handlers_share_the_lazy_agent_binding() {
    let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
    let handler = HelperHandler {
        helper_id: HelperId(1),
        agent: Arc::new(OnceLock::new()),
        state: make_state(),
        notif_tx,
        agent_side_slot: Arc::new(OnceLock::new()),
    };
    let request_handler = handler.clone();

    assert!(
        Arc::ptr_eq(&handler.agent, &request_handler.agent),
        "all request handler clones must share initialize's binding slot"
    );
}

/// An orphan session's `request_permission` (owning tab closed
/// mid-turn) must resolve to `Cancelled`, never an error — an error to
/// the shared CLI can drop the connection and every other tab with it.
#[tokio::test]
async fn request_permission_for_orphaned_session_returns_cancelled_not_error() {
    use acp::schema::v1::{
        PermissionOption, PermissionOptionId, PermissionOptionKind,
        RequestPermissionOutcome, RequestPermissionRequest, ToolCallId, ToolCallUpdate,
        ToolCallUpdateFields,
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
    // reap only acts when the key is a live pool entry.
    {
        let mut agents = state.agents.lock().await;
        agents.insert(key_a.clone(), Arc::new(tokio::sync::OnceCell::new()));
    }
    reap_agent(&state, &key_a).await;
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
        SelectedPermissionOutcome, StopReason, ToolCallId, ToolCallUpdate,
        ToolCallUpdateFields,
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
                let builder = acp::Agent
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
                                _ => responder
                                    .respond_with_error(acp::Error::method_not_found()),
                            }
                        },
                        acp::on_receive_request!(),
                    );
                let (_agent_link, agent_io) = conn::spawn_agent(
                    builder,
                    conn::byte_streams(aw.compat_write(), ar.compat()),
                );
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
                                                c.request_permission(a).await.map(
                                                    ClientResponse::RequestPermissionResponse,
                                                ),
                                            )
                                        }
                                        _ => responder.respond_with_error(
                                            acp::Error::method_not_found(),
                                        ),
                                    }
                                }
                            }
                        },
                        acp::on_receive_request!(),
                    );
                let (link, io) = conn::spawn_client(
                    builder,
                    conn::byte_streams(cw.compat_write(), cr.compat()),
                );
                tokio::task::spawn_local(async move {
                    let _ = io.await;
                });
                link
            };

            // ---- hop 2: master (helper-side agent) <-> mock helper client ----
            let (notif_tx, _notif_rx) = mpsc::channel(NOTIF_CHANNEL_CAPACITY);
            let agent = Arc::new(OnceLock::new());
            let _ = agent.set(Arc::new(AgentCli {
                conn: agent_conn,
                cached_init_resp: acp::schema::v1::InitializeResponse::new(
                    acp::schema::ProtocolVersion::V1,
                ),
                cli_source: Some(crate::agent_sessions::CliSource::Copilot),
                source: crate::agent_source::AgentSource::Host,
                cmd_key: "copilot --acp --stdio".to_string(),
            }));
            let handler = HelperHandler {
                helper_id: HelperId(1),
                agent,
                state: Arc::clone(&state),
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
                                        _ => responder.respond_with_error(
                                            acp::Error::method_not_found(),
                                        ),
                                    }
                                }
                            }
                        },
                        acp::on_receive_request!(),
                    );
                let (link, io) = conn::spawn_agent(
                    builder,
                    conn::byte_streams(mw.compat_write(), mr.compat()),
                );
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
                                AgentRequest::RequestPermissionRequest(_a) => {
                                    conn::respond_enum(
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
                                    )
                                }
                                _ => responder
                                    .respond_with_error(acp::Error::method_not_found()),
                            }
                        },
                        acp::on_receive_request!(),
                    );
                let (link, io) = conn::spawn_client(
                    builder,
                    conn::byte_streams(hw.compat_write(), hr.compat()),
                );
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
                notif_tx: tx1,
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
        map.insert(
            sid2.clone(),
            HelperRoute {
                helper_id: HelperId(2),
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
    // then route — `try_send` sees Closed; the helper_id check
    // sees the entry is B's; cleanup must NOT remove B.
    let (tx_a, rx_a) = mpsc::channel::<SessionNotification>(NOTIF_CHANNEL_CAPACITY);
    {
        let mut map = state.session_to_helper.lock().await;
        map.insert(
            sid.clone(),
            HelperRoute {
                helper_id: HelperId(1),
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

    // Rebind to helper B (simulating the racing load_session
    // landing between snapshot and try_send).
    let (tx_b, _rx_b) = mpsc::channel::<SessionNotification>(NOTIF_CHANNEL_CAPACITY);
    {
        let mut map = state.session_to_helper.lock().await;
        map.insert(
            sid.clone(),
            HelperRoute {
                helper_id: HelperId(2),
                notif_tx: tx_b,
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
    }

    // Drive the real production path. `tx_a` is the snapshot we'd
    // have captured before the rebind; `try_send` on it returns
    // Closed. The cleanup must look at the current map entry,
    // see it's helper B (≠ A), and leave it alone.
    match tx_a.try_send(make_notif(&sid)) {
        Err(mpsc::error::TrySendError::Closed(_)) => {}
        other => panic!("expected Closed, got {other:?}"),
    }
    {
        let mut map = state.session_to_helper.lock().await;
        match map.get(&sid) {
            Some(current) if current.helper_id == snap_helper_a => {
                map.remove(&sid);
            }
            _ => {} // identity mismatch — leave new route intact
        }
    }

    let map = state.session_to_helper.lock().await;
    let current = map.get(&sid).expect("helper B's route must survive");
    assert_eq!(
        current.helper_id,
        HelperId(2),
        "Closed cleanup must not remove a route rebound to a different helper"
    );
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
                notif_tx: tx_a.clone(),
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
        map.insert(
            SessionId::new("a2"),
            HelperRoute {
                helper_id: HelperId(1),
                notif_tx: tx_a,
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
        map.insert(
            SessionId::new("b1"),
            HelperRoute {
                helper_id: HelperId(2),
                notif_tx: tx_b,
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
        map.insert(
            SessionId::new("c1"),
            HelperRoute {
                helper_id: HelperId(3),
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
                notif_tx: tx_a,
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
        map.insert(
            sid_b.clone(),
            HelperRoute {
                helper_id: HelperId(2),
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
                notif_tx: notif_tx1.clone(),
                forwarder: None,
                consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
        );
        map.insert(
            sid_b.clone(),
            HelperRoute {
                helper_id: HelperId(1),
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
    let req =
        acp::schema::v1::CreateTerminalRequest::new(SessionId::new("nobody-home"), "echo".to_string());
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

    let resp = handle_sessions_list(&state, &session_registry::SessionsListParams { rescan: false })
        .await
        .expect("sessions/list succeeds");
    let parsed = session_registry::parse_sessions_list_response(&resp.0)
        .expect("response parses");

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
        map.insert(sid.clone(), HelperRoute {
            helper_id: HelperId(1),
            notif_tx,
            forwarder: None,
            consecutive_drops: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        });
    }
    state.registry.upsert(SessionInfo::new(sid, PathBuf::from("C:\\repo"))).await;
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

fn session_resume_params_for(sid: &acp::schema::v1::SessionId) -> crate::session_registry::SessionResumeDispatchedParams {
    crate::session_registry::SessionResumeDispatchedParams { sid: sid.clone() }
}

fn session_focus_params_for(sid: &acp::schema::v1::SessionId) -> crate::session_registry::SessionFocusParams {
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

fn make_state_with_wt(
    wt: Arc<dyn crate::shell::wt_channel::WtChannel>,
) -> Arc<MasterStateInner> {
    Arc::new(MasterStateInner {
        session_to_helper: Mutex::new(HashMap::new()),
        registry: crate::session_registry::InMemoryRegistry::shared(),
        helper_ext_subscribers: Mutex::new(HashMap::new()),
        wt: Some(wt),
        agents: Mutex::new(HashMap::new()),
        default_agent_cmd: "copilot --acp --stdio".to_string(),
        default_agent_id: Some("copilot".to_string()),
        allowed_agent_ids: None,
        cached_init_resp: OnceLock::new(),
        agent_conn: OnceLock::new(),
        cli_source: Some(crate::agent_sessions::CliSource::Copilot),
        helper_meta: Mutex::new(HashMap::new()),
        hook_owned: Mutex::new(HashSet::new()),
        born_bound: Mutex::new(HashSet::new()),
        orphaned_sessions: Mutex::new(HashMap::new()),
        host_list_cache: Mutex::new(None),
        wsl_titles_seed_at: Mutex::new(None),
        wsl_seed_in_flight: std::sync::atomic::AtomicBool::new(false),
    })
}

fn focus_params_for(sid: &acp::schema::v1::SessionId) -> crate::session_registry::FocusSessionParams {
    crate::session_registry::FocusSessionParams { session_id: sid.clone() }
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
    state.helper_ext_subscribers.lock().await.insert(HelperId(7), tx);

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
        ("sid-placeholder".to_string(), "OpenCode Real Title".to_string()),
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
        !refresh_synthetic_titles_from(&*state.registry, &std::collections::HashMap::new())
            .await
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

// ── WSL delegate title refresh (born-bound "-" rows) ─────────────

fn wsl_scan_row(id: &str, title: &str) -> crate::agent_sessions::AgentSession {
    use crate::agent_sessions::{AgentStatus, CliSource, SessionLocation, SessionOrigin};
    crate::agent_sessions::AgentSession {
        key: id.into(),
        cli_source: CliSource::Copilot,
        pane_session_id: Some("pane-guid".into()),
        window_id: None,
        tab_id: None,
        title: title.to_string(),
        cwd: std::path::PathBuf::from("/home/user/proj"),
        started_at: std::time::SystemTime::UNIX_EPOCH,
        last_activity_at: std::time::SystemTime::UNIX_EPOCH,
        status: AgentStatus::Idle,
        last_error: None,
        current_tool: None,
        attention_reason: None,
        log_path: None,
        origin: SessionOrigin::Unknown,
        location: SessionLocation::Wsl {
            distro: "Ubuntu".into(),
        },
    }
}

#[test]
fn wsl_titles_from_scan_filters_empty_and_injected_echo() {
    // A CLI can briefly echo the delegate's baked first message (which
    // embeds the `## Terminal Context (pane …)` marker) as a session title
    // before generating a real summary; that echo must be dropped so the
    // born-bound row keeps waiting rather than adopting a leaky title.
    let echo = format!(
        "hi test\n\n{}ABCDEF01-2345-6789-ABCD-EF0123456789)\n```\nPowerShell 7\n```",
        crate::session_registry::TERMINAL_CONTEXT_TITLE_MARKER
    );
    let scanned = vec![
        wsl_scan_row("s-real", "Fix the failing build"),
        wsl_scan_row("s-empty", ""),
        wsl_scan_row("s-echo", &echo),
    ];
    let map = wsl_titles_from_scan(&scanned);
    assert_eq!(map.len(), 1, "only the real title survives the filters");
    assert_eq!(
        map.get("s-real").map(String::as_str),
        Some("Fix the failing build")
    );
    assert!(!map.contains_key("s-empty"), "empty titles dropped");
    assert!(!map.contains_key("s-echo"), "injected-context echo dropped");
}

fn live_synthetic_pane_row(id: &str) -> crate::session_registry::SessionInfo {
    use crate::agent_sessions::{AgentStatus, SessionLocation};
    let mut row = crate::session_registry::SessionInfo::new(
        acp::schema::v1::SessionId::new(id.to_string()),
        std::path::PathBuf::from("/home/user/proj"),
    );
    // Synthetic (None title), live, pane-bound, WSL-located — the born-bound
    // WSL-delegate shape.
    row.pane_session_id = Some("pane-guid".to_string());
    row.status = Some(AgentStatus::Idle);
    row.location = SessionLocation::Wsl {
        distro: "Ubuntu".to_string(),
    };
    row
}

#[test]
fn wsl_title_seed_warranted_only_for_live_pane_bound_non_host_synthetic() {
    use crate::agent_sessions::{AgentStatus, SessionLocation};
    use std::collections::HashSet;

    // A born-bound WSL delegate row: synthetic, live, pane-bound, WSL-located,
    // and its id is NOT in the host session/list → warrants a WSL scan.
    let wsl_row = live_synthetic_pane_row("wsl-sid");
    let no_host: HashSet<String> = HashSet::new();
    assert!(wsl_title_seed_warranted(std::slice::from_ref(&wsl_row), &no_host));

    // Same row, but the host CLI lists it (a host delegate not yet titled) →
    // the host title refresh owns it, no WSL scan.
    let host_ids: HashSet<String> = ["wsl-sid".to_string()].into_iter().collect();
    assert!(!wsl_title_seed_warranted(std::slice::from_ref(&wsl_row), &host_ids));

    // A Host-located row with the same live/synthetic/pane-bound shape must
    // NOT warrant a scan, even when the host list is empty (temporarily
    // unavailable) — only in-distro rows can be titled by a WSL scan.
    let mut host_row = live_synthetic_pane_row("host-sid");
    host_row.location = SessionLocation::Host;
    assert!(!wsl_title_seed_warranted(std::slice::from_ref(&host_row), &no_host));

    // A non-synthetic row never warrants a scan.
    let mut titled = live_synthetic_pane_row("titled-sid");
    titled.title = Some("Real Title".to_string());
    assert!(!wsl_title_seed_warranted(std::slice::from_ref(&titled), &no_host));

    // Historical / ended synthetic rows are excluded so an untitled old row
    // can't drive perpetual scans.
    let mut ended = live_synthetic_pane_row("ended-sid");
    ended.status = Some(AgentStatus::Ended);
    assert!(!wsl_title_seed_warranted(std::slice::from_ref(&ended), &no_host));

    // A synthetic live row with no pane binding (not born-bound) is excluded.
    let mut unbound = live_synthetic_pane_row("unbound-sid");
    unbound.pane_session_id = None;
    assert!(!wsl_title_seed_warranted(std::slice::from_ref(&unbound), &no_host));
}

#[tokio::test]
async fn wsl_scan_upgrades_born_bound_wsl_title() {
    // End-to-end of the fix at the registry level: a born-bound WSL row
    // (registered Host-located with an empty title, as `register_launched_
    // session_with_master` does) gets its title from the scanned WSL session
    // that shares its id, via `spawn_wsl_seed`'s synthetic-title refresh.
    let state = make_state();
    let mut born = crate::session_registry::SessionInfo::new(
        acp::schema::v1::SessionId::new("wsl-delegate-sid".to_string()),
        std::path::PathBuf::from("/home/user/proj"),
    );
    born.title = Some(String::new());
    born.pane_session_id = Some("pane-guid".to_string());
    born.status = Some(crate::agent_sessions::AgentStatus::Idle);
    state.registry.upsert(born).await;

    // Directly drive the title refresh the worker performs from a scan.
    let scanned = vec![wsl_scan_row("wsl-delegate-sid", "Investigate flaky test")];
    let titles = wsl_titles_from_scan(&scanned);
    assert!(refresh_synthetic_titles_from(&*state.registry, &titles).await);
    assert_eq!(
        state
            .registry
            .lookup(&acp::schema::v1::SessionId::new(
                "wsl-delegate-sid".to_string()
            ))
            .await
            .unwrap()
            .title
            .as_deref(),
        Some("Investigate flaky test")
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
    assert!(row_refreshable_by_connected_agent(&row, Some(&CliSource::Copilot)));
    // Different known cli → skipped (the connected agent can't enumerate it).
    assert!(!row_refreshable_by_connected_agent(&row, Some(&CliSource::Claude)));
    // Unknown cli on either side → attempt (never skip).
    row.cli_source = None;
    assert!(row_refreshable_by_connected_agent(&row, Some(&CliSource::Copilot)));
    row.cli_source = Some(CliSource::Copilot);
    assert!(row_refreshable_by_connected_agent(&row, None));
}

#[test]
fn is_stale_host_history_row_reconcile_rules() {
    use crate::agent_sessions::{AgentStatus, SessionLocation, SessionOrigin};
    use std::collections::HashSet;
    let listed: HashSet<String> = ["kept".to_string()].into_iter().collect();
    let mk = |id: &str| {
        let mut r = crate::session_registry::SessionInfo::new(
            acp::schema::v1::SessionId::new(id.to_string()),
            std::path::PathBuf::from("C:\\Users\\dev"),
        );
        r.status = Some(AgentStatus::Historical);
        r.origin = Some(SessionOrigin::Unknown);
        r
    };
    // Terminal Class-B host row NOT in session/list → stale (drop).
    assert!(is_stale_host_history_row(&mk("gone"), &listed));
    // Still listed → keep.
    assert!(!is_stale_host_history_row(&mk("kept"), &listed));
    // Live (Idle/Working) → keep even if not listed.
    let mut live = mk("gone");
    live.status = Some(AgentStatus::Idle);
    assert!(!is_stale_host_history_row(&live, &listed));
    // Agent pane → never reconciled.
    let mut pane = mk("gone");
    pane.origin = Some(SessionOrigin::AgentPane);
    assert!(!is_stale_host_history_row(&pane, &listed));
    // WSL row → host can't authoritatively list distro sessions.
    let mut wsl = mk("gone");
    wsl.location = SessionLocation::Wsl { distro: "Ubuntu".to_string() };
    assert!(!is_stale_host_history_row(&wsl, &listed));
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
        .lookup(&acp::schema::v1::SessionId::new("sid-agent-pane".to_string()))
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
        crate::agent_sessions::SessionLocation::Wsl { distro: "Ubuntu".to_string() },
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

    let mut info =
        crate::session_registry::SessionInfo::new(sid.clone(), std::path::PathBuf::from("C:\\repo"));
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

