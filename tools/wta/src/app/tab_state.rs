use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::app_contracts::{PermOption, PlanEntry};
use crate::commands::{CommandSpec, MovePositionSpec};

use super::input_edit::InputHistory;
use super::{TabAutofixState, TurnState};

pub(crate) const DEFAULT_TAB_ID: &str = "0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoticeKind {
    Success,
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolCallKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    SwitchMode,
    #[default]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallOutput {
    pub text: String,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallLocation {
    pub path: String,
    #[serde(default)]
    pub line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolCallContent {
    Text(ToolCallOutput),
    Diff {
        path: String,
        #[serde(default)]
        old_text: Option<ToolCallOutput>,
        new_text: ToolCallOutput,
    },
    Terminal {
        id: String,
        #[serde(default)]
        output: Option<ToolCallOutput>,
        #[serde(default)]
        exit_code: Option<i64>,
    },
    Attachment {
        label: String,
        #[serde(default)]
        uri: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ChatMessage {
    User(String),
    Agent(String),
    /// Legacy untyped system message retained for persisted chat compatibility.
    System(String),
    Notice {
        kind: NoticeKind,
        text: String,
    },
    ToolCall {
        id: String,
        title: String,
        status: String,
        #[serde(default)]
        kind: ToolCallKind,
        /// Concise path/command hint pulled from the ACP tool call's
        /// `locations` or summarized `raw_input`. `None` when no useful
        /// target was reported or the title already states it verbatim.
        location: Option<String>,
        /// True when `location` is a shell command rather than a file path.
        /// Commands render on their own indented line below the title.
        #[serde(default)]
        location_is_command: bool,
        /// Working directory reported by the Agent for an execute tool.
        #[serde(default)]
        cwd: Option<String>,
        /// Bounded text reported through ACP tool-call content/raw output.
        #[serde(default)]
        output: Option<ToolCallOutput>,
        /// Process exit code, only when explicitly reported by the Agent.
        #[serde(default)]
        exit_code: Option<i64>,
        /// Standard ACP tool content, retained for expanded details.
        #[serde(default)]
        content: Vec<ToolCallContent>,
        /// All standard ACP locations, including optional line numbers.
        #[serde(default)]
        locations: Vec<ToolCallLocation>,
    },
    Plan(Vec<PlanEntry>),
    Error(String),
    /// Informational WT event surfaced inline in the chat (e.g. shell exit
    /// codes, OSC sequences). Distinct from `Error` so we can theme it
    /// differently and skip autofix wiring.
    AgentEvent(String),
    /// "Intelligent Terminal uses AI. Check for mistakes" disclaimer.
    /// Pushed on every agent-pane startup,
    /// no persistence gating — getting cleared by the next turn is fine,
    /// the next pane startup re-pushes it.
    Disclaimer,
}

impl ChatMessage {
    pub fn success(text: impl Into<String>) -> Self {
        Self::Notice {
            kind: NoticeKind::Success,
            text: text.into(),
        }
    }

    pub fn info(text: impl Into<String>) -> Self {
        Self::Notice {
            kind: NoticeKind::Info,
            text: text.into(),
        }
    }

    pub fn warning(text: impl Into<String>) -> Self {
        Self::Notice {
            kind: NoticeKind::Warning,
            text: text.into(),
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self::Notice {
            kind: NoticeKind::Error,
            text: text.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletedTurn {
    pub prompt: String,
    #[serde(default)]
    pub details: Vec<ChatMessage>,
    /// Whether the turn's `details` are visible in the UI. Tab to select +
    /// Enter to toggle. Default false (collapsed) so history stays compact.
    #[serde(default)]
    pub expanded: bool,
    /// Trailing inline status marker rendered in DIM next to the turn's
    /// first content line (e.g. "(canceled)" / "→ executed: Run Get-Date").
    /// Set when the user dismisses or executes a recommendation card, or
    /// cancels a mid-stream turn — `None` for normal chat turns.
    #[serde(default)]
    pub trailing_marker: Option<String>,
}

/// Maximum displayed characters for a collapsed turn header preview.
/// Picked so the `▶ > <preview>…` row stays well under a typical 120-col
/// wrap width even after the chevron + prompt prefix; longer prompts get
/// truncated with a trailing ellipsis. The full original text is always
/// preserved in the turn's first `details` entry.
const COLLAPSED_PROMPT_PREVIEW_CHARS: usize = 80;

/// Build the single-line preview shown in a collapsed `CompletedTurn`
/// header. Takes the first non-blank line of the prompt and clips it to
/// `COLLAPSED_PROMPT_PREVIEW_CHARS`. Multi-line prompts (system prompts,
/// pasted blocks, etc.) collapse to one row instead of wrapping over
/// dozens of lines in the chat scrollback.
pub fn collapsed_prompt_preview(text: &str) -> String {
    let first_line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let mut iter = first_line.chars();
    let mut out: String = (&mut iter).take(COLLAPSED_PROMPT_PREVIEW_CHARS).collect();
    // Append ellipsis if the prompt has more content than the preview
    // covered — either the first line itself was longer, or there are
    // additional non-empty lines below.
    let truncated = iter.next().is_some()
        || text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .nth(1)
            .is_some();
    if truncated {
        out.push('…');
    }
    out
}

pub struct PermissionState {
    pub tool_call_id: String,
    /// Fallback single-line text used when the panel cannot fit a full card.
    pub description: String,
    /// The agent's unmodified tool-call title.
    pub title: String,
    /// Locale-neutral icon derived from ACP `ToolKind`.
    pub kind_label: Option<String>,
    /// Concrete path, command, or URL shown in the full permission card.
    pub target: Option<String>,
    /// True when `target` is a shell command rather than a file path.
    pub target_is_command: bool,
    pub options: Vec<PermOption>,
    pub selected: usize,
    pub responder: Option<tokio::sync::oneshot::Sender<String>>,
}

pub struct UserInputState {
    pub request_id: String,
    pub request: crate::agent_tools::user_input::UserInputRequest,
    pub selected: usize,
    pub input: String,
    pub responder:
        Option<tokio::sync::oneshot::Sender<crate::agent_tools::user_input::UserInputResponse>>,
}

impl UserInputState {
    pub fn selection_count(&self) -> usize {
        self.request.choices.len() + usize::from(self.request.allow_freeform)
    }

    pub fn freeform_selected(&self) -> bool {
        self.request.allow_freeform && self.selected == self.request.choices.len()
    }
}

impl PermissionState {
    /// Index of the first "allow" option, used by the `y` quick-key and the
    /// `[Y]` button label.
    pub fn allow_index(&self) -> Option<usize> {
        self.options.iter().position(PermOption::is_allow)
    }

    /// Index of the first "reject" option, used by the `n` quick-key and the
    /// `[N]` button label.
    pub fn reject_index(&self) -> Option<usize> {
        self.options.iter().position(PermOption::is_reject)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum RecommendationFocus {
    #[default]
    Button,
    Input,
}

/// Single-axis scroll cursor. All mutations go through methods so callers
/// don't reinvent saturating-math; the upper bound `max` is established by
/// the layout/render pass once total content height is known and re-clamps
/// on every frame.
///
/// `by` deliberately does NOT clamp to `max` — the bound may be stale at
/// input time (the lazy chat build only learns `max` after exhausting
/// history). Clamping happens on the next `set_max`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Scroll {
    pub offset: usize,
    pub max: usize,
}

impl Scroll {
    pub fn by(&mut self, delta: isize) {
        self.offset = if delta >= 0 {
            self.offset.saturating_add(delta as usize)
        } else {
            self.offset.saturating_sub(delta.unsigned_abs())
        };
    }

    /// Jump to an absolute offset, clamped to current `max`. Only meaningful
    /// after `max` has been set this frame.
    pub fn set(&mut self, offset: usize) {
        self.offset = offset.min(self.max);
    }

    pub fn set_max(&mut self, max: usize) {
        self.max = max;
        if self.offset > max {
            self.offset = max;
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

pub(crate) struct PendingTerminalActionProposal {
    pub proposal_id: String,
    pub session_id: String,
    pub prompt_id: u64,
    pub is_autofix: bool,
    pub recommendations: super::RecommendationSet,
}

/// Everything that conceptually belongs to one tab's conversation: the
/// message history, the streaming buffer of the in-flight prompt, the
/// pending tool calls, the recommendations panel state, etc.
///
/// `App` holds a `HashMap<TabId, TabSession>` and a `tab_id` pointing at
/// the currently focused entry. Renderers read via `app.current_tab()`;
/// event handlers route updates to the relevant `TabSession` rather than
/// mutating shared `App` fields.
#[derive(Default)]
pub struct TabSession {
    /// Per-tab autofix state machine (see `TabAutofixState`).
    pub autofix: TabAutofixState,
    pub(crate) pending_terminal_action_proposal: Option<PendingTerminalActionProposal>,
    pub(crate) active_direct_proposal_id: Option<String>,
    pub usage: Option<crate::usage::UsageSnapshot>,
    pub usage_staleness: crate::usage::UsageStaleness,

    // Conversation history
    pub messages: Vec<ChatMessage>,
    pub completed_turns: Vec<CompletedTurn>,
    /// Tab/Shift+Tab selects a past turn (most recent first). Enter then
    /// toggles `CompletedTurn.expanded`. None means no selection — Enter
    /// goes to the input/prompt path as before.
    pub selected_completed_turn_idx: Option<usize>,
    pub chat_scroll: Scroll,

    // Streaming state
    pub pending_agent_response: String,
    /// Accumulator for `session/update` user_message_chunk events
    /// arriving during an ACP `session/load` replay (the historical
    /// user prompt for the next replayed turn). Flushed as a
    /// `ChatMessage::User` whenever a turn boundary is detected — an
    /// agent message / thought / tool call starts, OR the load
    /// completes (SessionAttached for the loading tab).
    pub pending_user_replay: String,
    /// True between the inbound `load_session` event and the
    /// `SessionAttached` event that closes out the ACP `session/load`
    /// call. While set, session/update chunk handlers accept chunks
    /// even though no `TurnState::Submitted` was created for the
    /// replay — `turn` stays Idle through the load.
    pub loading_session: bool,
    /// The session id we're currently loading into this tab, set when
    /// `loading_session` flips to true. The `SessionAttached` handler
    /// closes the replay window only when an attach event arrives whose
    /// `session_id` matches this value — otherwise an unrelated
    /// `SessionAttached` (e.g. the helper's bootstrap `session/new`
    /// that completed while a Plan-C `--initial-load-session-id` was
    /// still being processed) would prematurely flip `loading_session`
    /// off and the agent's replay chunks would be dropped at the chunk
    /// handlers' `if !loading_session { return; }` gate.
    pub loading_target_session_id: Option<String>,
    // Explicit per-turn lifecycle. Source of truth in the new state machine
    // (see `doc/specs/turn-state-refactor.md`).
    pub turn: TurnState,
    pub activity_frame: usize,
    /// Typewriter reveal cursor: how many characters of the *user-visible*
    /// streaming text are currently shown. The full text lives in
    /// `turn.buffer()`; the renderer only emits the first `reveal_chars`
    /// chars of it. Advanced toward the full length by `RevealTick`
    /// (`advance_reveal`), reset to 0 when a new turn starts streaming, and
    /// made irrelevant on finalize (the committed message renders in full).
    pub reveal_chars: usize,
    pub timing_note: Option<String>,
    pub selection_visible_pending: bool,

    // Tool calls / permission
    pub tool_calls: HashMap<String, (String, String)>,
    /// FIFO of pending permission requests for this session. The front
    /// entry is the one currently rendered and accepting keys; the rest
    /// queue up.
    pub permission: VecDeque<PermissionState>,
    /// FIFO of blocking clarification requests from the session MCP tool.
    pub user_input: VecDeque<UserInputState>,
    // Recommendation card UI focus (the set itself lives on
    // `turn.recommendations()`).
    pub selected_recommendation: usize,
    pub selected_button: usize,
    pub recommendation_focus: RecommendationFocus,
    pub rec_scroll: Scroll,
    pub rec_viewport_height: u16,

    /// Last value the helper published for this tab in a
    /// `set_agent_chip_target` event.
    pub last_emitted_chip_override: Option<String>,

    // Input editor state — per-tab so each tab keeps its own draft text,
    // cursor, and slash-command popup across switches.
    pub input: String,
    pub cursor_pos: usize,
    pub(super) input_history: InputHistory,
    pub(crate) attachments: super::attachments::PendingAttachments,
    /// True while a host-triggered text paste is reading the clipboard on a
    /// blocking worker.
    pub paste_pending: bool,
    /// Monotonic generation for async text paste.
    pub paste_generation: u64,
    /// Recomputed on every input mutation. Empty when not in
    /// command-prefix mode.
    pub command_popup_candidates: Vec<&'static CommandSpec>,
    /// Position candidates shown after `/move `.
    pub move_position_candidates: Vec<&'static MovePositionSpec>,
    /// Index into whichever popup candidate list is active.
    pub command_popup_selected: usize,

    // Filled in Milestone 2 once each tab has its own ACP SessionId.
    #[allow(dead_code)]
    pub session_id: Option<String>,

    /// Per-pane ACP model override, set by the `/model` picker.
    pub model_override: Option<String>,
    /// True while the `/model` picker modal is up for this tab.
    pub model_picker_open: bool,
    /// Highlighted row in the open model picker.
    pub model_picker_selected: usize,
    /// True while the `/agent` picker is open for this tab.
    pub agent_picker_open: bool,
    /// Highlighted row in `App::available_agents`.
    pub agent_picker_selected: usize,

    // agent session view (`/sessions`) — per-tab so each WT tab keeps
    // its own open/closed state and selected row across tab switches.
    pub current_view: View,
    pub agents_list_state: ratatui::widgets::ListState,
    pub agents_view: AgentsViewState,

    // "Does this tab want the agent pane visible?" — per-tab user intent.
    pub pane_open: bool,
    /// Transient position override for this tab's agent pane.
    pub agent_pane_position: Option<&'static str>,

    /// Pre-entry pane visibility, remembered when the user opens the
    /// session-management (Agents) view.
    pub agents_view_prev_pane_open: Option<bool>,
}

impl TabSession {
    pub fn scroll_to_bottom(&mut self) {
        self.chat_scroll.offset = 0;
    }

    pub(crate) fn should_show_thinking(&self) -> bool {
        self.turn.is_in_flight()
            && self.turn.recommendations().is_none()
            && self.permission.is_empty()
            && self.user_input.is_empty()
    }

    /// Whether the input box is the live, enterable caret target.
    pub fn input_has_nav_focus(&self) -> bool {
        self.selected_completed_turn_idx.is_none()
            && (self.turn.recommendations().is_none()
                || self.recommendation_focus == RecommendationFocus::Input)
            && self.permission.is_empty()
            && self.user_input.is_empty()
            && !self.paste_pending
            && !self.model_picker_open
            && !self.agent_picker_open
    }

    pub fn clear_recommendations(&mut self) {
        self.selected_recommendation = 0;
        self.selected_button = 0;
        self.recommendation_focus = RecommendationFocus::Button;
        self.rec_scroll.reset();
        self.rec_viewport_height = 0;
    }

    /// The pane the "Agent" chip should be pinned to while this tab has a
    /// recommendation card with a `Send` action selected.
    pub fn compute_chip_card_target(&self) -> Option<String> {
        if self.recommendation_focus == RecommendationFocus::Input {
            return None;
        }
        let recs = self.turn.recommendations()?;
        let choice = recs.choices.get(self.selected_recommendation)?;
        if choice
            .actions
            .iter()
            .any(|action| matches!(action, crate::coordinator::RecommendedAction::Send { .. }))
        {
            return self
                .turn
                .prompt()
                .and_then(|prompt| prompt.context.target_pane_id().map(str::to_string));
        }
        None
    }

    pub fn clear_chat_history(&mut self) {
        self.messages.clear();
        self.tool_calls.clear();
        self.permission.clear();
        self.user_input.clear();
        self.activity_frame = 0;
        self.pending_agent_response.clear();
        self.pending_user_replay.clear();
        self.chat_scroll.reset();
        self.timing_note = None;
        self.selection_visible_pending = false;
        self.turn = TurnState::Idle;
        self.clear_recommendations();
        self.attachments
            .remove_tokens_from_input(&mut self.input, &mut self.cursor_pos);
        self.clear_history_draft_attachments();
        self.paste_pending = false;
        self.paste_generation = self.paste_generation.wrapping_add(1);
    }

    pub fn flush_load_replay_pending(&mut self) {
        if !self.pending_user_replay.is_empty() {
            let text = std::mem::take(&mut self.pending_user_replay);
            self.messages.push(ChatMessage::User(text));
        }
        if !self.pending_agent_response.is_empty() {
            let text = std::mem::take(&mut self.pending_agent_response);
            self.messages.push(ChatMessage::Agent(text));
        }
    }

    pub fn flush_streamed_agent_segment(&mut self) {
        if let Some(text) = self.turn.take_buffer() {
            if !text.trim().is_empty() {
                self.messages.push(ChatMessage::Agent(text));
            }
            self.pending_agent_response.clear();
            self.reveal_chars = 0;
        }
    }

    pub fn pack_replayed_messages_into_turns(&mut self) {
        if self.messages.is_empty() {
            return;
        }
        let drained: Vec<ChatMessage> = std::mem::take(&mut self.messages);
        let mut kept: Vec<ChatMessage> = Vec::new();
        let mut current: Option<(String, Vec<ChatMessage>)> = None;
        for message in drained {
            match message {
                ChatMessage::User(text) => {
                    if let Some((prompt, details)) = current.take() {
                        self.completed_turns.push(CompletedTurn {
                            prompt,
                            details,
                            expanded: false,
                            trailing_marker: None,
                        });
                    }
                    let preview = collapsed_prompt_preview(&text);
                    let details = vec![ChatMessage::User(text)];
                    current = Some((preview, details));
                }
                other => {
                    if let Some((_, details)) = current.as_mut() {
                        details.push(other);
                    } else {
                        kept.push(other);
                    }
                }
            }
        }
        if let Some((prompt, details)) = current.take() {
            self.completed_turns.push(CompletedTurn {
                prompt,
                details,
                expanded: false,
                trailing_marker: None,
            });
        }
        self.messages = kept;
    }

    pub fn select_older_completed_turn(&mut self) {
        let len = self.completed_turns.len();
        if len == 0 {
            self.selected_completed_turn_idx = None;
            return;
        }
        self.selected_completed_turn_idx = match self.selected_completed_turn_idx {
            None => Some(len - 1),
            Some(0) => None,
            Some(index) => Some(index - 1),
        };
    }

    pub fn select_newer_completed_turn(&mut self) {
        let len = self.completed_turns.len();
        if len == 0 {
            self.selected_completed_turn_idx = None;
            return;
        }
        self.selected_completed_turn_idx = match self.selected_completed_turn_idx {
            None => Some(0),
            Some(index) if index + 1 >= len => None,
            Some(index) => Some(index + 1),
        };
    }

    pub fn toggle_selected_completed_turn(&mut self) {
        let Some(index) = self.selected_completed_turn_idx else {
            return;
        };
        if let Some(turn) = self.completed_turns.get_mut(index) {
            turn.expanded = !turn.expanded;
        }
    }

    pub fn current_turn_details(&self) -> Vec<ChatMessage> {
        self.messages
            .iter()
            .filter(|message| !matches!(message, ChatMessage::User(_)))
            .cloned()
            .collect()
    }
}

/// Top-level UI view selector. Toggled with Ctrl+Shift+/.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Chat,
    Agents,
}

impl Default for View {
    fn default() -> Self {
        View::Chat
    }
}

#[derive(Debug, Default, Clone)]
pub struct AgentsViewState {
    pub snapshot: Option<Vec<crate::session_registry::SessionInfo>>,
    pub focused_sid: Option<agent_client_protocol::schema::v1::SessionId>,
    pub search_query: String,
    pub search_focused: bool,
    pub refetch_in_flight: bool,
    pub dirty: bool,
    pub next_request_id: u64,
    pub latest_request_id: Option<u64>,
    pub pending_rescan: bool,
    pub rescan_in_flight: bool,
}
