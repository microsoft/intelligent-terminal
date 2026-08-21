#[cfg(test)]
use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

use crate::app_contracts::{PermOption, PlanEntry};
use crate::commands::{CommandSpec, MovePositionSpec};

use super::input_edit::InputHistory;
use super::{TabAutofixState, TurnState};

pub(crate) const DEFAULT_TAB_ID: &str = "0";

static NEXT_COMPLETED_TURN_LAYOUT_NAMESPACE: AtomicU64 = AtomicU64::new(1);
static NEXT_STREAMING_SOURCE_GENERATION: AtomicU64 = AtomicU64::new(1);
const MAX_COMPLETED_TURN_LAYOUT_CHANGES: usize = 2048;

pub(crate) struct CompletedTurnLayoutChanges {
    pub namespace: u64,
    pub generation: u64,
    pub len: usize,
    pub dirty_indices: Option<Vec<usize>>,
}

#[cfg(test)]
thread_local! {
    static STREAMING_GRAPHEME_FALLBACK_SCAN_COUNT: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_streaming_grapheme_fallback_scan_count() {
    STREAMING_GRAPHEME_FALLBACK_SCAN_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn streaming_grapheme_fallback_scan_count() -> usize {
    STREAMING_GRAPHEME_FALLBACK_SCAN_COUNT.with(Cell::get)
}

#[cfg(test)]
fn record_streaming_grapheme_fallback_scan() {
    STREAMING_GRAPHEME_FALLBACK_SCAN_COUNT.with(|count| count.set(count.get() + 1));
}

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
    /// "Intelligent Terminal uses AI." disclaimer.
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

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum ConfigPickerState {
    #[default]
    Closed,
    Options {
        selected: usize,
    },
    Values {
        option_id: String,
        selected: usize,
        parent_selected: Option<usize>,
    },
}

impl ConfigPickerState {
    pub fn is_open(&self) -> bool {
        !matches!(self, Self::Closed)
    }

    pub fn selected(&self) -> usize {
        match self {
            Self::Closed => 0,
            Self::Options { selected } | Self::Values { selected, .. } => *selected,
        }
    }

    pub fn option_id(&self) -> Option<&str> {
        match self {
            Self::Values { option_id, .. } => Some(option_id),
            _ => None,
        }
    }

    pub fn reconcile(&mut self, options: &[crate::app_contracts::AcpSessionConfigOption]) {
        let next = match std::mem::take(self) {
            Self::Closed => Self::Closed,
            Self::Options { selected } if !options.is_empty() => Self::Options {
                selected: selected.min(options.len() - 1),
            },
            Self::Values {
                option_id,
                selected,
                parent_selected,
            } => {
                let value_count = options
                    .iter()
                    .find(|option| option.id == option_id)
                    .map(|option| option.values.len());
                match value_count {
                    Some(value_count) if value_count > 0 => Self::Values {
                        option_id,
                        selected: selected.min(value_count - 1),
                        parent_selected,
                    },
                    _ => parent_selected
                        .filter(|_| !options.is_empty())
                        .map(|selected| Self::Options {
                            selected: selected.min(options.len() - 1),
                        })
                        .unwrap_or(Self::Closed),
                }
            }
            Self::Options { .. } => Self::Closed,
        };
        *self = next;
    }
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
    pub(crate) completed_turn_layout_namespace: u64,
    pub(crate) completed_turn_layout_ids: Vec<u64>,
    pub(crate) completed_turn_layout_revisions: Vec<u64>,
    pub(crate) next_completed_turn_layout_id: u64,
    pub(crate) completed_turn_layout_generation: u64,
    pub(crate) completed_turn_layout_changes: VecDeque<(u64, usize)>,
    /// Tab/Shift+Tab selects a past turn (most recent first). Enter then
    /// toggles `CompletedTurn.expanded`. None means no selection — Enter
    /// goes to the input/prompt path as before.
    pub selected_completed_turn_idx: Option<usize>,
    /// Set when keyboard navigation changes the completed-turn selection.
    /// The chat render pass consumes it after adjusting scroll just enough to
    /// reveal the selected turn.
    pub completed_turn_selection_visible_pending: bool,
    pub chat_scroll: Scroll,

    // Session replay state. These buffers are used only while loading_session
    // is true and never share storage with a live turn.
    pub replay_agent_buffer: String,
    pub replay_user_buffer: String,
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
    /// Typewriter reveal cursor, in extended grapheme clusters, for the final
    /// assistant-text item in the active transcript. Advanced toward its full length by `RevealTick`
    /// (`advance_reveal`), reset to 0 when a new turn starts streaming, and
    /// made irrelevant on finalize (the committed message renders in full).
    pub reveal_graphemes: usize,
    pub(crate) streaming_source_generation: u64,
    pub(crate) streaming_source_revision: u64,
    pub(crate) streaming_indexed_bytes: usize,
    pub(crate) streaming_grapheme_ends: Vec<usize>,
    pub timing_note: Option<String>,
    pub selection_visible_pending: bool,

    // Blocking action queues
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
    /// Navigation state for the ACP session configuration picker.
    pub config_picker: ConfigPickerState,
    /// Config option currently awaiting a `session/set_config_option` response.
    pub config_pending_id: Option<String>,
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
    fn clear_streaming_source_index(&mut self) {
        self.streaming_source_revision = 0;
        self.streaming_indexed_bytes = 0;
        self.streaming_grapheme_ends.clear();
    }

    fn start_streaming_source_index(&mut self, text: &str) {
        self.streaming_source_generation =
            NEXT_STREAMING_SOURCE_GENERATION.fetch_add(1, Ordering::Relaxed);
        self.streaming_source_revision = 1;
        self.streaming_indexed_bytes = text.len();
        self.streaming_grapheme_ends = text
            .grapheme_indices(true)
            .map(|(start, grapheme)| start + grapheme.len())
            .collect();
    }

    pub(crate) fn streaming_source_generation(&self) -> u64 {
        self.streaming_source_generation
    }

    pub(crate) fn streaming_source_revision(&self) -> u64 {
        self.streaming_source_revision
    }

    pub(crate) fn streaming_grapheme_count(&self) -> Option<usize> {
        let text = self.streaming_agent_text()?;
        if self.streaming_indexed_bytes == text.len() {
            Some(self.streaming_grapheme_ends.len())
        } else {
            #[cfg(test)]
            record_streaming_grapheme_fallback_scan();
            Some(text.graphemes(true).count())
        }
    }

    pub(crate) fn streaming_prefix_byte_end(&self, grapheme_count: usize) -> Option<usize> {
        let text = self.streaming_agent_text()?;
        if grapheme_count == 0 {
            return Some(0);
        }
        if self.streaming_indexed_bytes == text.len() {
            return Some(
                self.streaming_grapheme_ends
                    .get(grapheme_count - 1)
                    .copied()
                    .unwrap_or(text.len()),
            );
        }
        #[cfg(test)]
        record_streaming_grapheme_fallback_scan();
        Some(
            text.grapheme_indices(true)
                .nth(grapheme_count - 1)
                .map(|(start, grapheme)| start + grapheme.len())
                .unwrap_or(text.len()),
        )
    }

    fn record_completed_turn_layout_change(&mut self, index: usize) {
        if self.completed_turn_layout_generation == u64::MAX {
            self.completed_turn_layout_namespace =
                NEXT_COMPLETED_TURN_LAYOUT_NAMESPACE.fetch_add(1, Ordering::Relaxed);
            self.completed_turn_layout_generation = 1;
            self.completed_turn_layout_changes.clear();
        } else {
            self.completed_turn_layout_generation += 1;
        }
        self.completed_turn_layout_changes
            .push_back((self.completed_turn_layout_generation, index));
        while self.completed_turn_layout_changes.len() > MAX_COMPLETED_TURN_LAYOUT_CHANGES {
            self.completed_turn_layout_changes.pop_front();
        }
    }

    fn sync_completed_turn_layout_metadata(&mut self) {
        if self.completed_turn_layout_namespace == 0 {
            self.completed_turn_layout_namespace =
                NEXT_COMPLETED_TURN_LAYOUT_NAMESPACE.fetch_add(1, Ordering::Relaxed);
        }

        let len = self.completed_turns.len();
        if self.completed_turn_layout_ids.len() > len {
            self.completed_turn_layout_namespace =
                NEXT_COMPLETED_TURN_LAYOUT_NAMESPACE.fetch_add(1, Ordering::Relaxed);
            self.completed_turn_layout_generation = 0;
            self.completed_turn_layout_changes.clear();
        }
        self.completed_turn_layout_ids.truncate(len);
        self.completed_turn_layout_revisions.truncate(len);
        while self.completed_turn_layout_ids.len() < len {
            let index = self.completed_turn_layout_ids.len();
            self.next_completed_turn_layout_id =
                self.next_completed_turn_layout_id.wrapping_add(1).max(1);
            self.completed_turn_layout_ids
                .push(self.next_completed_turn_layout_id);
            self.completed_turn_layout_revisions.push(0);
            self.record_completed_turn_layout_change(index);
        }
    }

    pub(crate) fn completed_turn_layout_metadata(&mut self) -> (u64, Vec<(u64, u64)>) {
        self.sync_completed_turn_layout_metadata();

        (
            self.completed_turn_layout_namespace,
            self.completed_turn_layout_ids
                .iter()
                .copied()
                .zip(self.completed_turn_layout_revisions.iter().copied())
                .collect(),
        )
    }

    pub(crate) fn completed_turn_layout_changes_since(
        &mut self,
        previous: Option<(u64, u64)>,
    ) -> CompletedTurnLayoutChanges {
        self.sync_completed_turn_layout_metadata();
        let namespace = self.completed_turn_layout_namespace;
        let generation = self.completed_turn_layout_generation;
        let len = self.completed_turns.len();
        let Some((previous_namespace, previous_generation)) = previous else {
            return CompletedTurnLayoutChanges {
                namespace,
                generation,
                len,
                dirty_indices: None,
            };
        };
        if previous_namespace != namespace {
            return CompletedTurnLayoutChanges {
                namespace,
                generation,
                len,
                dirty_indices: None,
            };
        }
        if previous_generation == generation {
            return CompletedTurnLayoutChanges {
                namespace,
                generation,
                len,
                dirty_indices: Some(Vec::new()),
            };
        }

        let Some(first_retained_generation) = self
            .completed_turn_layout_changes
            .front()
            .map(|(generation, _)| *generation)
        else {
            return CompletedTurnLayoutChanges {
                namespace,
                generation,
                len,
                dirty_indices: None,
            };
        };
        if previous_generation.saturating_add(1) < first_retained_generation {
            return CompletedTurnLayoutChanges {
                namespace,
                generation,
                len,
                dirty_indices: None,
            };
        }

        let mut dirty_indices = self
            .completed_turn_layout_changes
            .iter()
            .filter(|(change_generation, _)| *change_generation > previous_generation)
            .map(|(_, index)| *index)
            .collect::<Vec<_>>();
        dirty_indices.sort_unstable();
        dirty_indices.dedup();
        CompletedTurnLayoutChanges {
            namespace,
            generation,
            len,
            dirty_indices: Some(dirty_indices),
        }
    }

    pub(crate) fn completed_turn_layout_identity(&mut self) -> (u64, u64) {
        self.sync_completed_turn_layout_metadata();
        (
            self.completed_turn_layout_namespace,
            self.completed_turn_layout_generation,
        )
    }

    pub(crate) fn completed_turn_layout_item(&self, index: usize) -> Option<(u64, u64)> {
        Some((
            *self.completed_turn_layout_ids.get(index)?,
            *self.completed_turn_layout_revisions.get(index)?,
        ))
    }

    pub(crate) fn mark_completed_turn_layout_dirty(&mut self, index: usize) {
        self.sync_completed_turn_layout_metadata();
        if let Some(revision) = self.completed_turn_layout_revisions.get_mut(index) {
            *revision = revision.wrapping_add(1);
            self.record_completed_turn_layout_change(index);
        }
    }

    pub fn clear_completed_turns(&mut self) {
        self.completed_turns.clear();
        self.completed_turn_layout_namespace = 0;
        self.completed_turn_layout_ids.clear();
        self.completed_turn_layout_revisions.clear();
        self.completed_turn_layout_generation = 0;
        self.completed_turn_layout_changes.clear();
    }

    pub fn set_last_completed_turn_trailing_marker(&mut self, marker: String) -> bool {
        let Some(index) = self.completed_turns.len().checked_sub(1) else {
            return false;
        };
        self.completed_turns[index].trailing_marker = Some(marker);
        self.mark_completed_turn_layout_dirty(index);
        true
    }

    pub fn set_completed_turn_expanded(&mut self, index: usize, expanded: bool) -> bool {
        let Some(turn) = self.completed_turns.get_mut(index) else {
            return false;
        };
        if turn.expanded != expanded {
            turn.expanded = expanded;
            self.mark_completed_turn_layout_dirty(index);
        }
        true
    }

    pub(crate) fn invalidate_pending_paste(&mut self) {
        self.paste_pending = false;
        self.paste_generation = self.paste_generation.wrapping_add(1);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.chat_scroll.offset = 0;
    }

    pub(crate) fn should_show_thinking(&self) -> bool {
        self.turn.is_in_flight()
            && self.turn.recommendations().is_none()
            && self.permission.is_empty()
            && self.user_input.is_empty()
            && self
                .streaming_agent_text()
                .is_none_or(|text| text.trim().is_empty())
            && !self.messages.iter().any(|message| {
                matches!(
                    message,
                    ChatMessage::ToolCall { status, .. }
                        if status.eq_ignore_ascii_case("pending")
                            || status.eq_ignore_ascii_case("inprogress")
                            || status.eq_ignore_ascii_case("running")
                )
            })
    }

    /// Whether the input box is the live, enterable caret target.
    pub fn input_has_nav_focus(&self) -> bool {
        self.selected_completed_turn_idx.is_none() && self.input_can_receive_nav_focus()
    }

    pub fn input_can_receive_nav_focus(&self) -> bool {
        (self.turn.recommendations().is_none()
            || self.recommendation_focus == RecommendationFocus::Input)
            && self.permission.is_empty()
            && self.user_input.is_empty()
            && !self.paste_pending
            && !self.model_picker_open
            && !self.config_picker.is_open()
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
        self.permission.clear();
        self.user_input.clear();
        self.activity_frame = 0;
        self.replay_agent_buffer.clear();
        self.replay_user_buffer.clear();
        self.clear_streaming_source_index();
        self.chat_scroll.reset();
        self.timing_note = None;
        self.selection_visible_pending = false;
        self.clear_completed_turn_selection();
        self.turn = TurnState::Idle;
        self.clear_recommendations();
        self.attachments
            .remove_tokens_from_input(&mut self.input, &mut self.cursor_pos);
        self.clear_history_draft_attachments();
        self.invalidate_pending_paste();
    }

    pub fn flush_load_replay_pending(&mut self) {
        if !self.replay_user_buffer.is_empty() {
            let text = std::mem::take(&mut self.replay_user_buffer);
            self.messages.push(ChatMessage::User(text));
        }
        if !self.replay_agent_buffer.is_empty() {
            let text = std::mem::take(&mut self.replay_agent_buffer);
            self.messages.push(ChatMessage::Agent(text));
        }
    }

    pub fn append_agent_chunk(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.messages.last_mut() {
            Some(ChatMessage::Agent(current)) => {
                let index_matches_source = self.streaming_indexed_bytes == current.len()
                    && self
                        .streaming_grapheme_ends
                        .last()
                        .is_none_or(|end| *end == current.len());
                let rescan_start = index_matches_source
                    .then(|| {
                        self.streaming_grapheme_ends
                            .len()
                            .checked_sub(2)
                            .and_then(|index| self.streaming_grapheme_ends.get(index).copied())
                            .unwrap_or(0)
                    })
                    .unwrap_or(0);
                current.push_str(text);
                if index_matches_source {
                    self.streaming_grapheme_ends
                        .retain(|end| *end <= rescan_start);
                    self.streaming_source_revision =
                        self.streaming_source_revision.wrapping_add(1).max(1);
                } else {
                    self.streaming_grapheme_ends.clear();
                    self.streaming_source_generation =
                        NEXT_STREAMING_SOURCE_GENERATION.fetch_add(1, Ordering::Relaxed);
                    self.streaming_source_revision = 1;
                }
                self.streaming_grapheme_ends.extend(
                    current[rescan_start..]
                        .grapheme_indices(true)
                        .map(|(start, grapheme)| rescan_start + start + grapheme.len()),
                );
                self.streaming_indexed_bytes = current.len();
            }
            _ => {
                self.messages.push(ChatMessage::Agent(text.to_string()));
                self.reveal_graphemes = 0;
                self.start_streaming_source_index(text);
            }
        }
    }

    pub fn streaming_agent_message_index(&self) -> Option<usize> {
        self.turn
            .is_streaming()
            .then(|| self.messages.len().checked_sub(1))
            .flatten()
            .filter(|index| matches!(self.messages.get(*index), Some(ChatMessage::Agent(_))))
    }

    pub fn streaming_agent_text(&self) -> Option<&str> {
        let index = self.streaming_agent_message_index()?;
        match self.messages.get(index) {
            Some(ChatMessage::Agent(text)) => Some(text),
            _ => None,
        }
    }

    pub fn active_agent_text(&self) -> String {
        self.messages
            .iter()
            .filter_map(|message| match message {
                ChatMessage::Agent(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn take_current_turn_details(&mut self) -> Vec<ChatMessage> {
        self.clear_streaming_source_index();
        std::mem::take(&mut self.messages)
            .into_iter()
            .filter(|message| !matches!(message, ChatMessage::User(_)))
            .collect()
    }

    pub fn pack_replayed_messages_into_turns(&mut self) {
        if self.messages.is_empty() {
            return;
        }
        self.clear_streaming_source_index();
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
            self.clear_completed_turn_selection();
            return;
        }
        self.selected_completed_turn_idx = match self.selected_completed_turn_idx {
            None => Some(len - 1),
            Some(0) => None,
            Some(index) => Some(index - 1),
        };
        self.completed_turn_selection_visible_pending = self.selected_completed_turn_idx.is_some();
    }

    pub fn select_newer_completed_turn(&mut self) {
        let len = self.completed_turns.len();
        if len == 0 {
            self.clear_completed_turn_selection();
            return;
        }
        self.selected_completed_turn_idx = match self.selected_completed_turn_idx {
            None => Some(0),
            Some(index) if index + 1 >= len => None,
            Some(index) => Some(index + 1),
        };
        self.completed_turn_selection_visible_pending = self.selected_completed_turn_idx.is_some();
    }

    pub fn clear_completed_turn_selection(&mut self) {
        self.selected_completed_turn_idx = None;
        self.completed_turn_selection_visible_pending = false;
    }

    pub fn select_completed_turn(&mut self, index: usize) -> bool {
        if index >= self.completed_turns.len() {
            return false;
        }
        self.selected_completed_turn_idx = Some(index);
        self.completed_turn_selection_visible_pending = true;
        true
    }

    pub fn toggle_completed_turn(&mut self, index: usize) -> bool {
        let Some(expanded) = self.completed_turns.get(index).map(|turn| turn.expanded) else {
            return false;
        };
        self.set_completed_turn_expanded(index, !expanded)
    }

    pub fn toggle_selected_completed_turn(&mut self) {
        let Some(index) = self.selected_completed_turn_idx else {
            return;
        };
        if self.toggle_completed_turn(index) {
            self.completed_turn_selection_visible_pending = true;
        }
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
