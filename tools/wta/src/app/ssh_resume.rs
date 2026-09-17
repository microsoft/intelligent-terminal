//! SSH helpers render versioned snapshots of the master's common session
//! registry. They never own the authoritative pane binding or launch a pane.

use super::*;
use crate::agent_sessions::{AgentSession, CliSource, SessionLocation};
use crate::ssh_session_registry::{Request, Snapshot, Source};

#[cfg(test)]
#[path = "ssh_resume_tests.rs"]
mod tests;

#[async_trait::async_trait(?Send)]
pub(crate) trait SshRegistryClient: Send + Sync {
    async fn request(&self, request: Request) -> Result<Snapshot>;
}

struct PipeRegistryClient {
    pipe: String,
}

#[async_trait::async_trait(?Send)]
impl SshRegistryClient for PipeRegistryClient {
    async fn request(&self, request: Request) -> Result<Snapshot> {
        let params = serde_json::value::to_raw_value(&request)?;
        let response = crate::cli::sessions::request_from_master(
            Some(self.pipe.clone()),
            agent_client_protocol::schema::v1::ExtRequest::new(
                crate::ssh_session_registry::METHOD,
                params.into(),
            ),
        )
        .await?;
        Ok(serde_json::from_str(response.0.get())?)
    }
}

#[derive(Debug, Clone)]
pub(crate) enum SshRegistryAction {
    History { tab_id: String, request_id: u64 },
    Cached,
    Activate { session_id: String },
}

struct CachedSnapshot {
    snapshot: Snapshot,
    sequence: u64,
    retired_epochs: HashSet<uuid::Uuid>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SshErrorOperation {
    History,
    Activate,
    Cached,
}

#[derive(Default)]
pub(crate) struct SshResumes {
    client: Option<Arc<dyn SshRegistryClient>>,
    snapshots: HashMap<Source, CachedSnapshot>,
    errors: HashMap<Source, SshErrorOperation>,
    pending_actions: HashMap<(Source, String), u64>,
    pending_cached: HashMap<Source, u64>,
    dirty_cached: HashSet<Source>,
    sequence: u64,
    last_poll: Option<std::time::Instant>,
}

impl SshResumes {
    fn next_sequence(&mut self) -> u64 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }

    fn accept(&mut self, snapshot: Snapshot, sequence: u64) -> Result<()> {
        let location = SessionLocation::Ssh {
            target: snapshot.source.target.clone(),
        };
        let cli = CliSource::from_agent_id(&snapshot.source.agent_id);
        anyhow::ensure!(
            cli.is_some()
                && snapshot.sessions.iter().all(|row| {
                    row.location == location && row.cli_source.as_ref() == cli.as_ref()
                }),
            "Shared SSH snapshot contains a foreign source."
        );
        if let Some(cached) = self.snapshots.get_mut(&snapshot.source) {
            if cached.retired_epochs.contains(&snapshot.epoch) {
                return Ok(());
            }
            if cached.snapshot.epoch == snapshot.epoch {
                if snapshot.revision < cached.snapshot.revision {
                    return Ok(());
                }
            } else {
                if sequence < cached.sequence {
                    return Ok(());
                }
                cached.retired_epochs.insert(cached.snapshot.epoch);
            }
            cached.sequence = cached.sequence.max(sequence);
            cached.snapshot = snapshot;
        } else {
            self.snapshots.insert(
                snapshot.source.clone(),
                CachedSnapshot {
                    snapshot,
                    sequence,
                    retired_epochs: HashSet::new(),
                },
            );
        }
        Ok(())
    }
}

impl App {
    pub(crate) fn set_sessions_master_pipe(&mut self, pipe: String) {
        self.ssh_resumes.client = Some(Arc::new(PipeRegistryClient { pipe }));
    }

    fn ssh_registry_context(
        &self,
    ) -> Result<(Arc<dyn SshRegistryClient>, mpsc::UnboundedSender<AppEvent>)> {
        let client = self.ssh_resumes.client.clone().ok_or_else(|| {
            anyhow::anyhow!("SSH session registry is not connected to the master.")
        })?;
        let sender = self.event_tx.clone().ok_or_else(|| {
            anyhow::anyhow!("SSH session registry requires the helper event loop.")
        })?;
        Ok((client, sender))
    }

    fn spawn_ssh_registry_request(
        &mut self,
        source: Source,
        action: SshRegistryAction,
        request: Request,
        sequence: u64,
    ) -> Result<tokio::task::AbortHandle> {
        let (client, sender) = self.ssh_registry_context()?;
        let task = tokio::task::spawn_local(async move {
            let result = client
                .request(request)
                .await
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send(AppEvent::SshRegistryResult {
                source,
                sequence,
                action,
                result,
            });
        });
        Ok(task.abort_handle())
    }

    pub(super) fn request_ssh_history(
        &mut self,
        tab_id: &str,
        request_id: u64,
        source: Source,
    ) -> Result<tokio::task::AbortHandle> {
        let sequence = self.ssh_resumes.next_sequence();
        self.ssh_resumes.last_poll = Some(std::time::Instant::now());
        self.spawn_ssh_registry_request(
            source.clone(),
            SshRegistryAction::History {
                tab_id: tab_id.to_string(),
                request_id,
            },
            Request::List {
                source,
                refresh_history: true,
            },
            sequence,
        )
    }

    pub(super) fn ssh_resume_pending(&self, session: &AgentSession) -> bool {
        let SessionLocation::Ssh { target } = &session.location else {
            return false;
        };
        let Some(agent_id) = known_cli_id(&session.cli_source) else {
            return false;
        };
        self.ssh_resumes.pending_actions.contains_key(&(
            Source {
                target: target.clone(),
                agent_id: agent_id.to_string(),
            },
            session.key.clone(),
        ))
    }

    pub(super) fn start_ssh_session_resume(&mut self, session: &AgentSession) -> Result<()> {
        let source = self
            .current_tab()
            .agents_view
            .ssh_source
            .clone()
            .ok_or_else(|| anyhow::anyhow!("SSH session has no profile source."))?;
        anyhow::ensure!(
            session.location
                == SessionLocation::Ssh {
                    target: source.target.clone()
                }
                && CliSource::from_agent_id(&source.agent_id).as_ref() == Some(&session.cli_source),
            "SSH session does not belong to the selected source."
        );
        let key = (source.clone(), session.key.clone());
        if self.ssh_resumes.pending_actions.contains_key(&key) {
            return Ok(());
        }
        let sequence = self.ssh_resumes.next_sequence();
        self.spawn_ssh_registry_request(
            source.clone(),
            SshRegistryAction::Activate {
                session_id: session.key.clone(),
            },
            Request::Activate {
                source: source.clone(),
                session_id: session.key.clone(),
            },
            sequence,
        )?;
        self.ssh_resumes.pending_actions.insert(key, sequence);
        self.set_shared_ssh_error(&source, None, SshErrorOperation::Activate);
        Ok(())
    }

    pub(super) fn request_cached_ssh_source(&mut self, source: &Source) {
        if !self.tab_sessions.values().any(|tab| {
            tab.current_view == View::Agents
                && tab.agents_view.snapshot.is_some()
                && tab.agents_view.ssh_source.as_ref() == Some(source)
        }) {
            return;
        }
        if self.ssh_resumes.pending_cached.contains_key(source) {
            self.ssh_resumes.dirty_cached.insert(source.clone());
            return;
        }
        let sequence = self.ssh_resumes.next_sequence();
        match self.spawn_ssh_registry_request(
            source.clone(),
            SshRegistryAction::Cached,
            Request::List {
                source: source.clone(),
                refresh_history: false,
            },
            sequence,
        ) {
            Ok(_) => {
                self.ssh_resumes
                    .pending_cached
                    .insert(source.clone(), sequence);
            }
            Err(error) => self.set_shared_ssh_error(
                source,
                Some(format!("{error:#}")),
                SshErrorOperation::Cached,
            ),
        }
    }

    pub(super) fn poll_shared_ssh_sessions(&mut self) {
        if self.ssh_resumes.client.is_none()
            || self
                .ssh_resumes
                .last_poll
                .is_some_and(|last| last.elapsed() < std::time::Duration::from_secs(5))
        {
            return;
        }
        self.ssh_resumes.last_poll = Some(std::time::Instant::now());
        let sources: HashSet<_> = self
            .tab_sessions
            .values()
            .filter(|tab| {
                tab.current_view == View::Agents
                    && tab.agents_view.snapshot.is_some()
                    && !tab.agents_view.refetch_in_flight
            })
            .filter_map(|tab| tab.agents_view.ssh_source.clone())
            .collect();
        for source in sources {
            self.request_cached_ssh_source(&source);
        }
    }

    pub(super) fn handle_ssh_registry_result(
        &mut self,
        source: Source,
        sequence: u64,
        action: SshRegistryAction,
        result: std::result::Result<Snapshot, String>,
    ) {
        match &action {
            SshRegistryAction::Activate { session_id } => {
                let key = (source.clone(), session_id.clone());
                if self.ssh_resumes.pending_actions.get(&key) != Some(&sequence) {
                    return;
                }
                self.ssh_resumes.pending_actions.remove(&key);
            }
            SshRegistryAction::Cached => {
                if self.ssh_resumes.pending_cached.get(&source) != Some(&sequence) {
                    return;
                }
                self.ssh_resumes.pending_cached.remove(&source);
            }
            SshRegistryAction::History { tab_id, request_id } => {
                if !self.tab_sessions.get(tab_id).is_some_and(|tab| {
                    tab.agents_view.snapshot.is_some()
                        && tab.agents_view.latest_request_id == Some(*request_id)
                        && tab.agents_view.ssh_source.as_ref() == Some(&source)
                }) {
                    return;
                }
            }
        }
        let result = result.and_then(|snapshot| {
            if snapshot.source != source {
                Err("SSH registry response did not match the requested source.".to_string())
            } else {
                self.ssh_resumes
                    .accept(snapshot, sequence)
                    .map_err(|error| format!("{error:#}"))
            }
        });
        let succeeded = result.is_ok();
        if let SshRegistryAction::History { tab_id, request_id } = &action {
            let rows = result.and_then(|()| {
                self.ssh_resumes
                    .snapshots
                    .get(&source)
                    .map(|cached| {
                        cached
                            .snapshot
                            .sessions
                            .iter()
                            .map(session_info_to_agent_session)
                            .collect()
                    })
                    .ok_or_else(|| "Shared SSH snapshot was not cached.".to_string())
            });
            if rows.is_err() {
                self.ssh_resumes
                    .errors
                    .insert(source.clone(), SshErrorOperation::History);
            } else {
                self.ssh_resumes.errors.remove(&source);
            }
            self.handle_ssh_sessions_loaded(
                tab_id,
                *request_id,
                &source.target,
                &source.agent_id,
                rows,
            );
        } else {
            let operation = if matches!(action, SshRegistryAction::Cached) {
                SshErrorOperation::Cached
            } else {
                SshErrorOperation::Activate
            };
            self.set_shared_ssh_error(&source, result.err(), operation);
        }
        if succeeded {
            self.refresh_ssh_resume_snapshots();
        }
        if matches!(action, SshRegistryAction::Cached)
            && self.ssh_resumes.dirty_cached.remove(&source)
        {
            self.request_cached_ssh_source(&source);
        }
        // A failed native operation may still have changed authoritative state
        // (for example, a missing pane becomes Ended). Re-read, never invent it.
        if matches!(action, SshRegistryAction::Activate { .. }) && !succeeded {
            self.request_cached_ssh_source(&source);
        }
    }

    fn set_shared_ssh_error(
        &mut self,
        source: &Source,
        error: Option<String>,
        operation: SshErrorOperation,
    ) {
        if let Some(error) = &error {
            tracing::warn!(target: "ssh_sessions", %error, "shared SSH session operation failed");
        }
        // A cached read only retries local registry access, not remote SSH or
        // a failed native launch. It must not erase those unrelated failures.
        if operation == SshErrorOperation::Cached
            && self
                .ssh_resumes
                .errors
                .get(source)
                .is_some_and(|prior| *prior != operation)
        {
            return;
        }
        if error.is_some() {
            self.ssh_resumes.errors.insert(source.clone(), operation);
        } else {
            self.ssh_resumes.errors.remove(source);
        }
        for tab in self.tab_sessions.values_mut() {
            if tab.agents_view.ssh_source.as_ref() == Some(source) {
                tab.agents_view.ssh_error.clone_from(&error);
            }
        }
    }

    pub(super) fn refresh_ssh_resume_snapshots(&mut self) {
        let mut selections = Vec::new();
        for (tab_id, tab) in &mut self.tab_sessions {
            let Some(source) = &tab.agents_view.ssh_source else {
                continue;
            };
            let Some(snapshot) = &mut tab.agents_view.snapshot else {
                continue;
            };
            let Some(cached) = self.ssh_resumes.snapshots.get(source) else {
                continue;
            };
            *snapshot = cached.snapshot.sessions.clone();
            selections.push((
                tab_id.clone(),
                tab.agents_list_state.selected().unwrap_or(0),
            ));
        }
        for (tab_id, selected) in selections {
            self.restore_agents_selection(&tab_id, selected);
        }
    }
}
