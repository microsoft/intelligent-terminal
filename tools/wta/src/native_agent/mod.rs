//! A minimal, backend-agnostic native ACP **agent**.
//!
//! This is WTA's own agent: it speaks the agent side of the Agent Client
//! Protocol over stdio (so `wta-master` spawns and drives it exactly like
//! Copilot/Claude), and behind it runs a single OpenAI-compatible chat
//! endpoint. Unlike a full agent CLI it ships a *tiny* system prompt and (for
//! now) **no tools**, which is the whole point: a small/slow local model can
//! actually keep up because we control prompt size and context, the two levers
//! Copilot doesn't expose.
//!
//! It is deliberately **not** local-only in its design — the backend is just an
//! OpenAI-compatible `base_url`/`api_key`/`model`. The first shipping step wires
//! it to a local provider (Ollama / Foundry Local), but pointing it at a cloud
//! endpoint is purely configuration.
//!
//! Scope today (L0): chat only. `prompt` collects the user text, calls the
//! backend, streams the reply back as a single `AgentMessageChunk`, and ends
//! the turn. Tool calls / editing are future steps.

use agent_client_protocol as acp;
use agent_client_protocol::{Agent, Client};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use tokio::sync::OnceCell;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

mod openai;

/// Backend connection settings for the native agent. Backend-agnostic: a local
/// provider and a cloud provider differ only by these values.
#[derive(Debug, Clone)]
pub struct NativeAgentConfig {
    /// OpenAI-compatible base URL, e.g. `http://127.0.0.1:11434/v1`.
    pub base_url: String,
    /// API key. Local providers ignore it but the OpenAI contract wants one.
    pub api_key: String,
    /// The model id to send to the backend.
    pub model: String,
}

impl NativeAgentConfig {
    /// Resolve the backend config: explicit flags win, else the `COPILOT_PROVIDER_*`
    /// env (BYOK), else a local Ollama default. Keeps the native agent
    /// backend-agnostic — local just happens to be the first default.
    pub fn resolve(
        base_url: Option<String>,
        model: Option<String>,
        api_key: Option<String>,
    ) -> Self {
        let env = crate::llm_provider::LlmProviderConfig::from_env();
        let base_url = base_url
            .filter(|s| !s.trim().is_empty())
            .or(env.base_url)
            .unwrap_or_else(|| "http://127.0.0.1:11434/v1".to_string());
        let model = model
            .filter(|s| !s.trim().is_empty())
            .or(env.model)
            .unwrap_or_else(|| "qwen2.5:0.5b".to_string());
        let api_key = api_key
            .filter(|s| !s.trim().is_empty())
            .or(env.api_key)
            .unwrap_or_else(|| "local".to_string());
        Self { base_url, api_key, model }
    }
}

/// One conversation turn kept for context. Roles map to OpenAI chat roles.
#[derive(Clone)]
struct Turn {
    role: &'static str,
    content: String,
}

/// The native agent state: a backend config, the live model (switchable), and
/// per-session chat history. `conn` is populated immediately after the
/// connection is built so `prompt` can stream replies back.
struct NativeAgent {
    conn: Rc<OnceCell<Rc<acp::AgentSideConnection>>>,
    base_url: String,
    api_key: String,
    model: RefCell<String>,
    history: RefCell<HashMap<String, Vec<Turn>>>,
}

/// Tiny system prompt — kept intentionally short so small-context local models
/// have room for the actual conversation.
const SYSTEM_PROMPT: &str =
    "You are a concise terminal assistant. Answer briefly and directly. \
     You have no tools; if asked to run a command, suggest the command text.";

/// Cap on retained turns per session, so a long chat can't overflow a small
/// local context window.
const MAX_HISTORY_TURNS: usize = 12;

impl NativeAgent {
    fn new(cfg: NativeAgentConfig) -> Self {
        Self {
            conn: Rc::new(OnceCell::new()),
            base_url: cfg.base_url,
            api_key: cfg.api_key,
            model: RefCell::new(cfg.model),
            history: RefCell::new(HashMap::new()),
        }
    }

    fn model_state(&self) -> acp::SessionModelState {
        let id = self.model.borrow().clone();
        acp::SessionModelState::new(
            id.clone(),
            vec![acp::ModelInfo::new(id.clone(), id).description("Native (BYOK) model")],
        )
    }
}

/// Extract the user-visible text from a prompt's content blocks.
fn prompt_text(blocks: &[acp::ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            acp::ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[async_trait::async_trait(?Send)]
impl Agent for NativeAgent {
    async fn initialize(
        &self,
        args: acp::InitializeRequest,
    ) -> acp::Result<acp::InitializeResponse> {
        Ok(acp::InitializeResponse::new(args.protocol_version).agent_info(
            acp::Implementation::new("wta-native", env!("CARGO_PKG_VERSION"))
                .title("WTA Native Agent"),
        ))
    }

    async fn authenticate(
        &self,
        _args: acp::AuthenticateRequest,
    ) -> acp::Result<acp::AuthenticateResponse> {
        Ok(acp::AuthenticateResponse::default())
    }

    async fn new_session(
        &self,
        _args: acp::NewSessionRequest,
    ) -> acp::Result<acp::NewSessionResponse> {
        let id = format!("wta-native-{}", uuid_like());
        self.history.borrow_mut().insert(id.clone(), Vec::new());
        Ok(acp::NewSessionResponse::new(acp::SessionId::new(id.as_str()))
            .models(Some(self.model_state())))
    }

    async fn set_session_model(
        &self,
        args: acp::SetSessionModelRequest,
    ) -> acp::Result<acp::SetSessionModelResponse> {
        *self.model.borrow_mut() = args.model_id.0.to_string();
        Ok(acp::SetSessionModelResponse::default())
    }

    async fn prompt(&self, args: acp::PromptRequest) -> acp::Result<acp::PromptResponse> {
        let sid = args.session_id.0.to_string();
        let user = prompt_text(&args.prompt);

        // Build the request: system + capped history + this turn.
        let mut msgs: Vec<openai::ChatMessage> = vec![openai::ChatMessage::new("system", SYSTEM_PROMPT)];
        if let Some(turns) = self.history.borrow().get(&sid) {
            for t in turns.iter() {
                msgs.push(openai::ChatMessage::new(t.role, &t.content));
            }
        }
        msgs.push(openai::ChatMessage::new("user", &user));

        let base = self.base_url.clone();
        let key = self.api_key.clone();
        let model = self.model.borrow().clone();
        // The HTTP probe is blocking; run it off the single-threaded ACP loop.
        let reply = tokio::task::spawn_blocking(move || {
            openai::chat_completion(&base, &key, &model, &msgs)
        })
        .await
        .unwrap_or_else(|e| Err(format!("join error: {e}")))
        .unwrap_or_else(|e| format!("[native agent error] {e}"));

        // Record the turn (capped) for the next prompt's context.
        {
            let mut hist = self.history.borrow_mut();
            let turns = hist.entry(sid.clone()).or_default();
            turns.push(Turn { role: "user", content: user });
            turns.push(Turn { role: "assistant", content: reply.clone() });
            let overflow = turns.len().saturating_sub(MAX_HISTORY_TURNS);
            if overflow > 0 {
                turns.drain(0..overflow);
            }
        }

        if let Some(conn) = self.conn.get() {
            let _ = conn
                .session_notification(acp::SessionNotification::new(
                    args.session_id,
                    acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(
                        reply.as_str().into(),
                    )),
                ))
                .await;
        }
        Ok(acp::PromptResponse::new(acp::StopReason::EndTurn))
    }

    async fn cancel(&self, _args: acp::CancelNotification) -> acp::Result<()> {
        Ok(())
    }
}

/// A cheap unique-ish id without pulling in the `uuid` crate.
fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}

/// Run the native agent: serve ACP over stdio until the client disconnects.
pub async fn run(cfg: NativeAgentConfig) -> anyhow::Result<()> {
    tracing::info!(target: "native_agent", base_url=%cfg.base_url, model=%cfg.model, "native agent starting");
    let agent = NativeAgent::new(cfg);
    let conn_slot = agent.conn.clone();

    let stdout = tokio::io::stdout().compat_write();
    let stdin = tokio::io::stdin().compat();
    let (conn, io_task) = acp::AgentSideConnection::new(agent, stdout, stdin, |fut| {
        tokio::task::spawn_local(fut);
    });
    // Hand the connection back to the agent so `prompt` can stream replies.
    let _ = conn_slot.set(Rc::new(conn));
    io_task
        .await
        .map_err(|e| anyhow::anyhow!("native agent io: {e}"))
}
