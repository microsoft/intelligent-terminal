use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::fmt;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use uuid::Uuid;

pub const CHANNEL_VERSION: &str = "v1";
pub const PIPE_PREFIX: &str = r"\\.\pipe\IntelligentTerminal.Proposal.";
const MAX_VALIDATION_RETRIES: u8 = 2;
const MAX_TOMBSTONES: usize = 4;
const TOMBSTONE_TTL: Duration = Duration::from_secs(3 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProposalChannel {
    helper_instance_id: Uuid,
    turn_nonce: Uuid,
}

impl ProposalChannel {
    fn new(helper_instance_id: Uuid) -> Self {
        Self {
            helper_instance_id,
            turn_nonce: Uuid::new_v4(),
        }
    }

    pub fn pipe_name(&self) -> String {
        format!("{PIPE_PREFIX}{:x}", self.helper_instance_id.simple())
    }
}

impl fmt::Display for ProposalChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{CHANNEL_VERSION}.{:x}.{:x}",
            self.helper_instance_id.simple(),
            self.turn_nonce.simple()
        )
    }
}

impl FromStr for ProposalChannel {
    type Err = ChannelParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut parts = value.split('.');
        let version = parts.next().ok_or(ChannelParseError)?;
        let helper = parts.next().ok_or(ChannelParseError)?;
        let turn = parts.next().ok_or(ChannelParseError)?;
        if parts.next().is_some()
            || version != CHANNEL_VERSION
            || !is_lower_hex_uuid(helper)
            || !is_lower_hex_uuid(turn)
        {
            return Err(ChannelParseError);
        }
        Ok(Self {
            helper_instance_id: Uuid::parse_str(helper).map_err(|_| ChannelParseError)?,
            turn_nonce: Uuid::parse_str(turn).map_err(|_| ChannelParseError)?,
        })
    }
}

fn is_lower_hex_uuid(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelParseError;

impl fmt::Display for ChannelParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected v1.<32 lowercase hex>.<32 lowercase hex>")
    }
}

impl std::error::Error for ChannelParseError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalBinding {
    pub session_id: String,
    pub prompt_id: u64,
    pub active_target: Option<String>,
    pub is_autofix: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalChannelState {
    Issued,
    Validating,
    AwaitingUser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalValidationStatus {
    Accepted,
    UnknownChannel,
    HelperMismatch,
    Stale,
    Superseded,
    AlreadyConsumed,
    InvalidSchema,
    Rejected,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalFinalStatus {
    Confirmed,
    Cancelled,
    Superseded,
    TimedOut,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelFailure {
    pub status: ProposalValidationStatus,
    pub reason: &'static str,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationContext {
    pub proposal_id: String,
    pub binding: ProposalBinding,
}

struct ActiveChannel {
    channel: ProposalChannel,
    binding: ProposalBinding,
    state: ProposalChannelState,
    validation_retries: u8,
    proposal_id: Option<String>,
    final_responder: Option<oneshot::Sender<ProposalFinalStatus>>,
}

#[derive(Debug, Clone, Copy)]
struct Tombstone {
    channel_hash: [u8; 32],
    status: Option<ProposalFinalStatus>,
    created_at: Instant,
}

pub struct ConfirmationClaim {
    channel_hash: [u8; 32],
    final_responder: oneshot::Sender<ProposalFinalStatus>,
}

struct ChannelState {
    pipe_available: bool,
    agent_transport_available: bool,
    active: Option<ActiveChannel>,
    tombstones: VecDeque<Tombstone>,
}

pub struct ProposalChannelManager {
    helper_instance_id: Uuid,
    state: Mutex<ChannelState>,
}

impl ProposalChannelManager {
    pub fn new() -> Self {
        Self {
            helper_instance_id: Uuid::new_v4(),
            state: Mutex::new(ChannelState {
                pipe_available: true,
                agent_transport_available: true,
                active: None,
                tombstones: VecDeque::new(),
            }),
        }
    }

    pub fn pipe_name(&self) -> String {
        format!("{PIPE_PREFIX}{:x}", self.helper_instance_id.simple())
    }

    pub fn issue(
        &self,
        session_id: String,
        prompt_id: u64,
        active_target: Option<String>,
        is_autofix: bool,
    ) -> Result<ProposalChannel, ChannelFailure> {
        let mut state = self.lock_state();
        self.prune_tombstones(&mut state);
        if !state.pipe_available || !state.agent_transport_available {
            return Err(failure(
                ProposalValidationStatus::Unavailable,
                "proposal transport is unavailable",
                false,
            ));
        }
        self.invalidate_active(&mut state, ProposalFinalStatus::Superseded);
        let channel = ProposalChannel::new(self.helper_instance_id);
        state.active = Some(ActiveChannel {
            channel: channel.clone(),
            binding: ProposalBinding {
                session_id,
                prompt_id,
                active_target,
                is_autofix,
            },
            state: ProposalChannelState::Issued,
            validation_retries: 0,
            proposal_id: None,
            final_responder: None,
        });
        Ok(channel)
    }

    pub fn validate_permission(
        &self,
        session_id: &str,
        channel: &ProposalChannel,
    ) -> Result<(), ChannelFailure> {
        let mut state = self.lock_state();
        self.prune_tombstones(&mut state);
        self.ensure_local_channel(channel)?;
        if !state.pipe_available || !state.agent_transport_available {
            return Err(failure(
                ProposalValidationStatus::Unavailable,
                "proposal transport is unavailable",
                false,
            ));
        }
        let Some(active) = state.active.as_mut() else {
            return Err(self.inactive_failure(&state, channel));
        };
        if active.channel != *channel {
            return Err(self.inactive_failure(&state, channel));
        }
        if active.binding.session_id != session_id {
            return Err(failure(
                ProposalValidationStatus::Stale,
                "channel does not belong to the requesting ACP session",
                false,
            ));
        }
        if active.state != ProposalChannelState::Issued {
            return Err(failure(
                ProposalValidationStatus::AlreadyConsumed,
                "channel is already being validated or awaiting the user",
                false,
            ));
        }
        Ok(())
    }

    pub fn begin_validation(
        &self,
        channel: &ProposalChannel,
    ) -> Result<ValidationContext, ChannelFailure> {
        let mut state = self.lock_state();
        self.prune_tombstones(&mut state);
        self.ensure_local_channel(channel)?;
        let transport_available = state.pipe_available && state.agent_transport_available;
        let Some(active) = state.active.as_mut() else {
            return Err(self.inactive_failure(&state, channel));
        };
        if active.channel != *channel {
            return Err(self.inactive_failure(&state, channel));
        }
        if !transport_available {
            return Err(failure(
                ProposalValidationStatus::Unavailable,
                "proposal transport is unavailable",
                false,
            ));
        }
        if active.state != ProposalChannelState::Issued {
            return Err(failure(
                ProposalValidationStatus::AlreadyConsumed,
                "channel is already being validated or awaiting the user",
                false,
            ));
        }
        let proposal_id = Uuid::new_v4().to_string();
        active.state = ProposalChannelState::Validating;
        active.proposal_id = Some(proposal_id.clone());
        Ok(ValidationContext {
            proposal_id,
            binding: active.binding.clone(),
        })
    }

    pub fn accept_validation(
        &self,
        proposal_id: &str,
        final_responder: oneshot::Sender<ProposalFinalStatus>,
    ) -> bool {
        let mut state = self.lock_state();
        let Some(active) = state.active.as_mut() else {
            return false;
        };
        if active.state != ProposalChannelState::Validating
            || active.proposal_id.as_deref() != Some(proposal_id)
        {
            return false;
        }
        active.state = ProposalChannelState::AwaitingUser;
        active.final_responder = Some(final_responder);
        true
    }

    pub fn reject_validation(&self, proposal_id: &str, retryable: bool) -> bool {
        let mut state = self.lock_state();
        let Some(active) = state.active.as_mut() else {
            return false;
        };
        if active.state != ProposalChannelState::Validating
            || active.proposal_id.as_deref() != Some(proposal_id)
        {
            return false;
        }
        active.validation_retries = active.validation_retries.saturating_add(1);
        let can_retry = retryable && active.validation_retries <= MAX_VALIDATION_RETRIES;
        if can_retry {
            active.state = ProposalChannelState::Issued;
            active.proposal_id = None;
        } else {
            self.invalidate_active(&mut state, ProposalFinalStatus::Cancelled);
        }
        can_retry
    }

    pub fn claim_confirmation(&self, proposal_id: &str) -> Option<ConfirmationClaim> {
        let mut state = self.lock_state();
        let active = state.active.as_ref()?;
        if active.state != ProposalChannelState::AwaitingUser
            || active.proposal_id.as_deref() != Some(proposal_id)
            || active.final_responder.is_none()
        {
            return None;
        }
        let mut active = state.active.take()?;
        let final_responder = active.final_responder.take()?;
        let channel_hash = channel_hash(&active.channel);
        state.tombstones.push_back(Tombstone {
            channel_hash,
            status: None,
            created_at: Instant::now(),
        });
        self.prune_tombstones(&mut state);
        Some(ConfirmationClaim {
            channel_hash,
            final_responder,
        })
    }

    pub fn finalize_confirmation(&self, claim: ConfirmationClaim, status: ProposalFinalStatus) {
        let mut state = self.lock_state();
        if let Some(tombstone) = state
            .tombstones
            .iter_mut()
            .rev()
            .find(|item| item.channel_hash == claim.channel_hash)
        {
            tombstone.status = Some(status);
            tombstone.created_at = Instant::now();
        } else {
            state.tombstones.push_back(Tombstone {
                channel_hash: claim.channel_hash,
                status: Some(status),
                created_at: Instant::now(),
            });
        }
        self.prune_tombstones(&mut state);
        drop(state);
        let _ = claim.final_responder.send(status);
    }

    pub fn resolve_final(&self, proposal_id: &str, status: ProposalFinalStatus) -> bool {
        let mut state = self.lock_state();
        let matches = state
            .active
            .as_ref()
            .is_some_and(|active| active.proposal_id.as_deref() == Some(proposal_id));
        if !matches {
            return false;
        }
        self.invalidate_active(&mut state, status);
        true
    }

    pub fn set_pipe_available(&self, available: bool) {
        let mut state = self.lock_state();
        if !available {
            self.invalidate_active(&mut state, ProposalFinalStatus::Unavailable);
        }
        state.pipe_available = available;
    }

    pub fn set_agent_transport_available(&self, available: bool) {
        let mut state = self.lock_state();
        if !available {
            self.invalidate_active(&mut state, ProposalFinalStatus::Unavailable);
        }
        state.agent_transport_available = available;
    }

    #[cfg(test)]
    fn active_state(&self) -> Option<ProposalChannelState> {
        self.lock_state().active.as_ref().map(|active| active.state)
    }

    fn ensure_local_channel(&self, channel: &ProposalChannel) -> Result<(), ChannelFailure> {
        if channel.helper_instance_id != self.helper_instance_id {
            return Err(failure(
                ProposalValidationStatus::HelperMismatch,
                "channel belongs to another Helper",
                false,
            ));
        }
        Ok(())
    }

    fn inactive_failure(&self, state: &ChannelState, channel: &ProposalChannel) -> ChannelFailure {
        let hash = channel_hash(channel);
        if let Some(tombstone) = state
            .tombstones
            .iter()
            .rev()
            .find(|item| item.channel_hash == hash)
        {
            let (status, reason) = match tombstone.status {
                Some(ProposalFinalStatus::Superseded) => (
                    ProposalValidationStatus::Superseded,
                    "channel was superseded by a newer turn",
                ),
                None | Some(ProposalFinalStatus::Unavailable) => (
                    ProposalValidationStatus::Unavailable,
                    "owning Helper became unavailable",
                ),
                _ => (
                    ProposalValidationStatus::AlreadyConsumed,
                    "channel already reached a terminal state",
                ),
            };
            return failure(status, reason, false);
        }
        failure(
            ProposalValidationStatus::UnknownChannel,
            "channel is not active on this Helper",
            false,
        )
    }

    fn invalidate_active(&self, state: &mut ChannelState, status: ProposalFinalStatus) {
        let Some(mut active) = state.active.take() else {
            return;
        };
        if let Some(responder) = active.final_responder.take() {
            let _ = responder.send(status);
        }
        state.tombstones.push_back(Tombstone {
            channel_hash: channel_hash(&active.channel),
            status: Some(status),
            created_at: Instant::now(),
        });
        self.prune_tombstones(state);
    }

    fn prune_tombstones(&self, state: &mut ChannelState) {
        let now = Instant::now();
        while state.tombstones.front().is_some_and(|item| {
            now.saturating_duration_since(item.created_at) >= TOMBSTONE_TTL
        }) {
            state.tombstones.pop_front();
        }
        while state.tombstones.len() > MAX_TOMBSTONES {
            state.tombstones.pop_front();
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, ChannelState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for ProposalChannelManager {
    fn default() -> Self {
        Self::new()
    }
}

fn channel_hash(channel: &ProposalChannel) -> [u8; 32] {
    Sha256::digest(channel.to_string().as_bytes()).into()
}

fn failure(
    status: ProposalValidationStatus,
    reason: &'static str,
    retryable: bool,
) -> ChannelFailure {
    ChannelFailure {
        status,
        reason,
        retryable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_round_trips_and_derives_pipe() {
        let manager = ProposalChannelManager::new();
        let channel = manager
            .issue("session".into(), 7, Some("pane".into()), false)
            .unwrap();
        let encoded = channel.to_string();
        assert_eq!(encoded.parse::<ProposalChannel>().unwrap(), channel);
        assert_eq!(channel.pipe_name(), manager.pipe_name());
        assert_eq!(encoded.len(), 68);
    }

    #[test]
    fn channel_parser_rejects_noncanonical_forms() {
        let manager = ProposalChannelManager::new();
        let channel = manager
            .issue("session".into(), 1, None, false)
            .unwrap()
            .to_string();
        assert!(channel
            .to_ascii_uppercase()
            .parse::<ProposalChannel>()
            .is_err());
        assert!(channel
            .replace("v1.", "v2.")
            .parse::<ProposalChannel>()
            .is_err());
        assert!(format!("{channel}.extra")
            .parse::<ProposalChannel>()
            .is_err());
    }

    #[test]
    fn validation_accepts_current_channel_without_permission() {
        let manager = ProposalChannelManager::new();
        let channel = manager.issue("session".into(), 1, None, false).unwrap();
        let context = manager.begin_validation(&channel).unwrap();
        assert_eq!(context.binding.prompt_id, 1);
        assert_eq!(
            manager.active_state(),
            Some(ProposalChannelState::Validating)
        );
    }

    #[test]
    fn accepted_proposal_resolves_waiting_cli() {
        let manager = ProposalChannelManager::new();
        let channel = manager.issue("session".into(), 1, None, false).unwrap();
        let context = manager.begin_validation(&channel).unwrap();
        let (tx, rx) = oneshot::channel();
        assert!(manager.accept_validation(&context.proposal_id, tx));
        assert!(manager.resolve_final(&context.proposal_id, ProposalFinalStatus::Confirmed));
        assert_eq!(rx.blocking_recv().unwrap(), ProposalFinalStatus::Confirmed);
    }

    #[test]
    fn newer_turn_supersedes_old_channel() {
        let manager = ProposalChannelManager::new();
        let old = manager.issue("session".into(), 1, None, false).unwrap();
        let _new = manager.issue("session".into(), 2, None, false).unwrap();
        let failure = manager.begin_validation(&old).unwrap_err();
        assert_eq!(failure.status, ProposalValidationStatus::Superseded);
    }

    #[test]
    fn schema_retry_returns_channel_to_issued() {
        let manager = ProposalChannelManager::new();
        let channel = manager.issue("session".into(), 1, None, false).unwrap();
        let context = manager.begin_validation(&channel).unwrap();
        assert!(manager.reject_validation(&context.proposal_id, true));
        assert_eq!(manager.active_state(), Some(ProposalChannelState::Issued));
        assert!(manager.begin_validation(&channel).is_ok());
    }

    #[test]
    fn permission_validation_does_not_gate_or_consume_submission() {
        let manager = ProposalChannelManager::new();
        let channel = manager.issue("session".into(), 1, None, false).unwrap();
        manager.validate_permission("session", &channel).unwrap();
        assert_eq!(manager.active_state(), Some(ProposalChannelState::Issued));
        assert!(manager.begin_validation(&channel).is_ok());
    }

    #[test]
    fn agent_reconnect_does_not_revive_failed_pipe() {
        let manager = ProposalChannelManager::new();
        manager.set_pipe_available(false);
        manager.set_agent_transport_available(false);
        manager.set_agent_transport_available(true);

        let failure = manager.issue("session".into(), 1, None, false).unwrap_err();
        assert_eq!(failure.status, ProposalValidationStatus::Unavailable);
    }

    #[test]
    fn agent_reconnect_restores_channels_when_pipe_is_live() {
        let manager = ProposalChannelManager::new();
        manager.set_agent_transport_available(false);
        assert!(manager.issue("session".into(), 1, None, false).is_err());

        manager.set_agent_transport_available(true);
        assert!(manager.issue("session".into(), 2, None, false).is_ok());
    }
}
