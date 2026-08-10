//! Bounded reconnect replay for authoritative relay envelopes.

use std::collections::VecDeque;

use super::relay::RelayEnvelope;

/// In-memory, bounded event history. It is intentionally not persisted: after
/// a process restart the preserved `serverSeq` forces reconnecting clients to
/// obtain fresh snapshots rather than trusting a partial historical stream.
#[derive(Debug)]
pub(crate) struct ReplayBuffer {
    capacity: usize,
    entries: VecDeque<RelayEnvelope>,
}

impl ReplayBuffer {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: VecDeque::with_capacity(capacity),
        }
    }

    pub(crate) fn push(&mut self, envelope: RelayEnvelope) {
        if self.capacity == 0 {
            return;
        }
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(envelope);
    }

    /// Returns `None` when the gap predates the bounded buffer. Otherwise
    /// returns the events after `last_seen_server_seq` matching subscriptions.
    pub(crate) fn replay_since(
        &self,
        last_seen_server_seq: u64,
        current_server_seq: u64,
        subscriptions: &[String],
    ) -> Option<Vec<RelayEnvelope>> {
        if last_seen_server_seq >= current_server_seq {
            return Some(Vec::new());
        }

        let oldest = self.entries.front().map(|entry| entry.server_seq)?;
        if last_seen_server_seq.saturating_add(1) < oldest {
            return None;
        }

        Some(
            self.entries
                .iter()
                .filter(|entry| {
                    entry.server_seq > last_seen_server_seq
                        && subscriptions.iter().any(|uri| uri == &entry.channel)
                })
                .cloned()
                .collect(),
        )
    }
}
