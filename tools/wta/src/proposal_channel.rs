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

#[derive(Debug, Clone, Copy)]
pub struct ProposalChannelConfig {
    pub armed_lease: Duration,
    pub max_validation_retries: u8,
    pub max_tombstones: usize,
    pub tombstone_ttl: Duration,
}

impl Default for ProposalChannelConfig {
    fn default() -> Self {
        Self {
            armed_lease: Duration::from_secs(30),
            max_validation_retries: 2,
            max_tombstones: 4,
            tombstone_ttl: Duration::from_secs(3 * 60),
        }
    }
}

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
    pub session_epoch: u64,
    pub prompt_id: u64,
    pub active_target: Option<String>,
    pub is_autofix: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalChannelState {
    Issued,
    Armed,
    Validating,
    AwaitingUser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalValidationStatus {
    Accepted,
    UnknownChannel,
    HelperMismatch,
    NotArmed,
    Stale,
    Superseded,
    Expired,
    DigestMismatch,
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
    SessionReplaced,
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
    pub channel: ProposalChannel,
    pub binding: ProposalBinding,
}

struct ActiveChannel {
    channel: ProposalChannel,
    binding: ProposalBinding,
    state: ProposalChannelState,
    validation_retries: u8,
    payload_digest: Option<[u8; 32]>,
    armed_until: Option<Instant>,
    proposal_id: Option<String>,
    final_responder: Option<oneshot::Sender<ProposalFinalStatus>>,
}

#[derive(Debug, Clone, Copy)]
struct Tombstone {
    channel_hash: [u8; 32],
    status: ProposalFinalStatus,
    created_at: Instant,
}

struct ChannelState {
    session_epoch: u64,
    transport_available: bool,
    active: Option<ActiveChannel>,
    tombstones: VecDeque<Tombstone>,
}

pub struct ProposalChannelManager {
    helper_instance_id: Uuid,
    config: ProposalChannelConfig,
    state: Mutex<ChannelState>,
}

impl ProposalChannelManager {
    pub fn new() -> Self {
        Self::with_config(ProposalChannelConfig::default())
    }

    fn with_config(config: ProposalChannelConfig) -> Self {
        Self {
            helper_instance_id: Uuid::new_v4(),
            config,
            state: Mutex::new(ChannelState {
                session_epoch: 0,
                transport_available: true,
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
        if !state.transport_available {
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
                session_epoch: state.session_epoch,
                prompt_id,
                active_target,
                is_autofix,
            },
            state: ProposalChannelState::Issued,
            validation_retries: 0,
            payload_digest: None,
            armed_until: None,
            proposal_id: None,
            final_responder: None,
        });
        Ok(channel)
    }

    pub fn arm(
        &self,
        session_id: &str,
        channel: &ProposalChannel,
        payload: &[u8],
    ) -> Result<(), ChannelFailure> {
        let mut state = self.lock_state();
        self.prune_tombstones(&mut state);
        self.ensure_local_channel(channel)?;
        if !state.transport_available {
            return Err(failure(
                ProposalValidationStatus::Unavailable,
                "proposal transport is unavailable",
                false,
            ));
        }
        let session_epoch = state.session_epoch;
        let Some(active) = state.active.as_mut() else {
            return Err(self.inactive_failure(&state, channel));
        };
        if active.channel != *channel {
            return Err(self.inactive_failure(&state, channel));
        }
        if active.binding.session_epoch != session_epoch || active.binding.session_id != session_id
        {
            return Err(failure(
                ProposalValidationStatus::Stale,
                "channel does not belong to the requesting ACP session",
                false,
            ));
        }
        if active.state != ProposalChannelState::Issued {
            return Err(failure(
                ProposalValidationStatus::AlreadyConsumed,
                "channel is already armed or consumed",
                false,
            ));
        }
        active.payload_digest = Some(payload_digest(payload));
        active.armed_until = Some(Instant::now() + self.config.armed_lease);
        active.state = ProposalChannelState::Armed;
        Ok(())
    }

    pub fn begin_validation(
        &self,
        channel: &ProposalChannel,
        payload: &[u8],
    ) -> Result<ValidationContext, ChannelFailure> {
        let mut state = self.lock_state();
        self.prune_tombstones(&mut state);
        self.ensure_local_channel(channel)?;
        let session_epoch = state.session_epoch;
        let transport_available = state.transport_available;
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
        if active.binding.session_epoch != session_epoch {
            return Err(failure(
                ProposalValidationStatus::Stale,
                "channel belongs to a replaced session",
                false,
            ));
        }
        if active.state != ProposalChannelState::Armed {
            let (status, reason) = if active.state == ProposalChannelState::Issued {
                (
                    ProposalValidationStatus::NotArmed,
                    "channel was not approved for this payload",
                )
            } else {
                (
                    ProposalValidationStatus::AlreadyConsumed,
                    "channel is already being validated or awaiting the user",
                )
            };
            return Err(failure(status, reason, false));
        }
        if active
            .armed_until
            .is_none_or(|deadline| deadline <= Instant::now())
        {
            active.state = ProposalChannelState::Issued;
            active.payload_digest = None;
            active.armed_until = None;
            return Err(failure(
                ProposalValidationStatus::Expired,
                "channel approval lease expired",
                true,
            ));
        }
        if active.payload_digest != Some(payload_digest(payload)) {
            active.validation_retries = active.validation_retries.saturating_add(1);
            let can_retry = active.validation_retries <= self.config.max_validation_retries;
            if can_retry {
                active.state = ProposalChannelState::Issued;
                active.payload_digest = None;
                active.armed_until = None;
            } else {
                self.invalidate_active(&mut state, ProposalFinalStatus::Cancelled);
            }
            return Err(failure(
                ProposalValidationStatus::DigestMismatch,
                "payload differs from the approved command",
                can_retry,
            ));
        }
        let proposal_id = Uuid::new_v4().to_string();
        active.state = ProposalChannelState::Validating;
        active.proposal_id = Some(proposal_id.clone());
        Ok(ValidationContext {
            proposal_id,
            channel: active.channel.clone(),
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
        let can_retry =
            retryable && active.validation_retries <= self.config.max_validation_retries;
        if can_retry {
            active.state = ProposalChannelState::Issued;
            active.payload_digest = None;
            active.armed_until = None;
            active.proposal_id = None;
        } else {
            self.invalidate_active(&mut state, ProposalFinalStatus::Cancelled);
        }
        can_retry
    }

    pub fn claim_confirmation(
        &self,
        proposal_id: &str,
    ) -> Option<oneshot::Sender<ProposalFinalStatus>> {
        let mut state = self.lock_state();
        let active = state.active.as_ref()?;
        if active.state != ProposalChannelState::AwaitingUser
            || active.proposal_id.as_deref() != Some(proposal_id)
            || active.final_responder.is_none()
        {
            return None;
        }
        let mut active = state.active.take()?;
        let responder = active.final_responder.take()?;
        state.tombstones.push_back(Tombstone {
            channel_hash: channel_hash(&active.channel),
            status: ProposalFinalStatus::Confirmed,
            created_at: Instant::now(),
        });
        self.prune_tombstones(&mut state);
        Some(responder)
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

    pub fn replace_session(&self) {
        let mut state = self.lock_state();
        self.invalidate_active(&mut state, ProposalFinalStatus::SessionReplaced);
        state.session_epoch = state.session_epoch.wrapping_add(1);
    }

    pub fn cancel_active(&self) {
        let mut state = self.lock_state();
        self.invalidate_active(&mut state, ProposalFinalStatus::Cancelled);
    }

    pub fn set_transport_available(&self, available: bool) {
        let mut state = self.lock_state();
        if !available {
            self.invalidate_active(&mut state, ProposalFinalStatus::Unavailable);
        }
        state.transport_available = available;
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
                ProposalFinalStatus::Superseded => (
                    ProposalValidationStatus::Superseded,
                    "channel was superseded by a newer turn",
                ),
                ProposalFinalStatus::SessionReplaced => (
                    ProposalValidationStatus::Stale,
                    "channel belongs to a replaced session",
                ),
                ProposalFinalStatus::Unavailable => (
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
            status,
            created_at: Instant::now(),
        });
        self.prune_tombstones(state);
    }

    fn prune_tombstones(&self, state: &mut ChannelState) {
        let now = Instant::now();
        while state.tombstones.front().is_some_and(|item| {
            now.saturating_duration_since(item.created_at) >= self.config.tombstone_ttl
        }) {
            state.tombstones.pop_front();
        }
        while state.tombstones.len() > self.config.max_tombstones {
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

fn payload_digest(payload: &[u8]) -> [u8; 32] {
    Sha256::digest(payload).into()
}

fn channel_hash(channel: &ProposalChannel) -> [u8; 32] {
    payload_digest(channel.to_string().as_bytes())
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

    fn manager() -> ProposalChannelManager {
        ProposalChannelManager::with_config(ProposalChannelConfig {
            armed_lease: Duration::from_secs(30),
            max_validation_retries: 2,
            max_tombstones: 4,
            tombstone_ttl: Duration::from_secs(180),
        })
    }

    #[test]
    fn channel_round_trips_and_derives_pipe() {
        let manager = manager();
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
        let manager = manager();
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
    fn validation_requires_matching_permission_digest() {
        let manager = manager();
        let channel = manager.issue("session".into(), 1, None, false).unwrap();
        let unarmed = manager.begin_validation(&channel, b"payload").unwrap_err();
        assert_eq!(unarmed.status, ProposalValidationStatus::NotArmed);

        manager.arm("session", &channel, b"payload").unwrap();
        let mismatch = manager.begin_validation(&channel, b"changed").unwrap_err();
        assert_eq!(mismatch.status, ProposalValidationStatus::DigestMismatch);
        assert!(mismatch.retryable);
        assert_eq!(manager.active_state(), Some(ProposalChannelState::Issued));

        manager.arm("session", &channel, b"payload").unwrap();
        let context = manager.begin_validation(&channel, b"payload").unwrap();
        assert_eq!(context.binding.prompt_id, 1);
        assert_eq!(
            manager.active_state(),
            Some(ProposalChannelState::Validating)
        );
    }

    #[test]
    fn accepted_proposal_resolves_waiting_cli() {
        let manager = manager();
        let channel = manager.issue("session".into(), 1, None, false).unwrap();
        manager.arm("session", &channel, b"payload").unwrap();
        let context = manager.begin_validation(&channel, b"payload").unwrap();
        let (tx, rx) = oneshot::channel();
        assert!(manager.accept_validation(&context.proposal_id, tx));
        assert!(manager.resolve_final(&context.proposal_id, ProposalFinalStatus::Confirmed));
        assert_eq!(rx.blocking_recv().unwrap(), ProposalFinalStatus::Confirmed);
    }

    #[test]
    fn newer_turn_supersedes_old_channel() {
        let manager = manager();
        let old = manager.issue("session".into(), 1, None, false).unwrap();
        let _new = manager.issue("session".into(), 2, None, false).unwrap();
        let failure = manager.begin_validation(&old, b"payload").unwrap_err();
        assert_eq!(failure.status, ProposalValidationStatus::Superseded);
    }

    #[test]
    fn schema_retry_returns_channel_to_issued() {
        let manager = manager();
        let channel = manager.issue("session".into(), 1, None, false).unwrap();
        manager.arm("session", &channel, b"bad").unwrap();
        let context = manager.begin_validation(&channel, b"bad").unwrap();
        assert!(manager.reject_validation(&context.proposal_id, true));
        assert_eq!(manager.active_state(), Some(ProposalChannelState::Issued));
        manager.arm("session", &channel, b"fixed").unwrap();
        assert!(manager.begin_validation(&channel, b"fixed").is_ok());
    }

    #[test]
    fn session_replacement_returns_stale_tombstone() {
        let manager = manager();
        let channel = manager.issue("session".into(), 1, None, false).unwrap();
        manager.replace_session();
        let failure = manager.begin_validation(&channel, b"payload").unwrap_err();
        assert_eq!(failure.status, ProposalValidationStatus::Stale);
    }

    #[test]
    fn expired_arm_can_be_approved_again() {
        let manager = ProposalChannelManager::with_config(ProposalChannelConfig {
            armed_lease: Duration::ZERO,
            ..ProposalChannelConfig::default()
        });
        let channel = manager.issue("session".into(), 1, None, false).unwrap();
        manager.arm("session", &channel, b"payload").unwrap();
        let failure = manager.begin_validation(&channel, b"payload").unwrap_err();
        assert_eq!(failure.status, ProposalValidationStatus::Expired);
        assert!(failure.retryable);
        assert_eq!(manager.active_state(), Some(ProposalChannelState::Issued));
    }
}
