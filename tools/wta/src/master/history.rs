//! Disposable ACP history connection and retained single-flight requests.

use super::*;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::time::Instant;

use crate::protocol::acp::history_process::{HistoryJob, WslHistoryGroup};

const WAIT_STEPS: [u64; 5] = [5, 10, 20, 40, 60];
const HARD_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone)]
pub(super) struct HistoryTarget {
    pub command: String,
    pub agent_id: String,
    pub source: crate::agent_source::AgentSource,
    pub provider: ProviderBinding,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_wait_requires_three_fast_successes_per_reduction() {
        let mut wait = AdaptiveWait::default();
        wait.timed_out();
        assert_eq!(wait.budget(), Duration::from_secs(10));
        wait.success(Duration::from_secs(8));
        assert_eq!(wait.budget(), Duration::from_secs(10));
        for _ in 0..2 {
            wait.success(Duration::from_millis(250));
        }
        assert_eq!(wait.budget(), Duration::from_secs(10));
        wait.success(Duration::from_secs(5));
        assert_eq!(wait.fast_successes, 0, "slow success breaks the streak");
        for _ in 0..3 {
            wait.success(Duration::from_millis(250));
        }
        assert_eq!(wait.budget(), Duration::from_secs(5));
        for expected in [10, 20, 40, 60, 60] {
            wait.timed_out();
            assert_eq!(wait.budget(), Duration::from_secs(expected));
        }
        for expected in [40, 20, 10, 5] {
            for _ in 0..3 {
                wait.success(Duration::from_millis(250));
            }
            assert_eq!(wait.budget(), Duration::from_secs(expected));
        }
    }
}

#[derive(Default)]
pub(super) struct AdaptiveWait {
    step: usize,
    fast_successes: u8,
}

impl AdaptiveWait {
    fn budget(&self) -> Duration {
        Duration::from_secs(WAIT_STEPS[self.step])
    }

    fn timed_out(&mut self) {
        self.fast_successes = 0;
        self.step = (self.step + 1).min(WAIT_STEPS.len() - 1);
    }

    fn success(&mut self, elapsed: Duration) {
        while elapsed >= self.budget() && self.step < WAIT_STEPS.len() - 1 {
            self.timed_out();
        }
        if elapsed >= HISTORY_REFRESH_INTERVAL {
            self.fast_successes = 0;
        } else {
            self.fast_successes += 1;
            if self.fast_successes == 3 {
                self.step = self.step.saturating_sub(1);
                self.fast_successes = 0;
            }
        }
    }
}

pub(super) struct HistoryProcess {
    conn: Option<conn::ClientLink>,
    child: Option<tokio::process::Child>,
    job: Option<HistoryJob>,
    wsl_group: Option<WslHistoryGroup>,
    wsl_bootstrap: Option<(String, String)>,
    incoming: Option<BufReader<tokio::process::ChildStdout>>,
    receipt: Vec<u8>,
    stderr: AgentStderrLog,
    init_timeout: Duration,
    initialized: bool,
    io: Option<tokio::task::JoinHandle<acp::Result<()>>>,
    #[cfg(test)]
    reject_stop: bool,
}

impl HistoryProcess {
    fn spawn(target: &HistoryTarget) -> Result<Self> {
        let mut spawned = crate::protocol::acp::spawn::spawn_history_agent_process(
            &target.command,
            &target.agent_id,
            &target.source,
            target.provider.spawn_selection(),
        )?;
        let stderr = AgentStderrLog::new(format!("history-{}", target.agent_id));
        if let Some(pipe) = spawned.child.stderr.take() {
            stderr.drain(pipe);
        }
        let incoming = BufReader::new(
            spawned
                .child
                .stdout
                .take()
                .ok_or_else(|| anyhow!("history stdout missing"))?,
        );
        let wsl_bootstrap = match (&target.source, spawned.wsl_history_marker) {
            (crate::agent_source::AgentSource::Wsl { distro }, Some(marker)) => {
                Some((distro.clone(), marker))
            }
            _ => None,
        };
        Ok(Self {
            conn: None,
            child: Some(spawned.child),
            job: spawned.history_job,
            wsl_group: None,
            wsl_bootstrap,
            incoming: Some(incoming),
            receipt: Vec::new(),
            stderr,
            init_timeout: Duration::from_secs(if spawned.is_npx { 60 } else { 15 }),
            initialized: false,
            io: None,
            #[cfg(test)]
            reject_stop: false,
        })
    }

    async fn read_wsl_receipt(&mut self) -> Result<()> {
        if let Some((distro, marker)) = &self.wsl_bootstrap {
            let incoming = self
                .incoming
                .as_mut()
                .ok_or_else(|| anyhow!("WSL history bootstrap pipe missing"))?;
            let count = tokio::time::timeout(
                Duration::from_secs(15),
                incoming.read_until(b'\n', &mut self.receipt),
            )
            .await
            .context("WSL history ownership receipt timed out")??;
            if count != 0 || !self.receipt.is_empty() {
                self.wsl_group = Some(WslHistoryGroup::parse(
                    distro,
                    marker,
                    std::str::from_utf8(&self.receipt)?,
                )?);
            }
            // EOF before the trusted bootstrap prints its receipt means no
            // agent was launched; malformed/missing live receipts fail closed.
            self.wsl_bootstrap = None;
        }
        Ok(())
    }

    async fn initialize(&mut self, target: &HistoryTarget) -> Result<()> {
        self.read_wsl_receipt().await?;
        let result = async {
            let child = self.child.as_mut().ok_or_else(|| anyhow!("history child missing"))?;
            let outgoing = child.stdin.take().ok_or_else(|| anyhow!("history stdin missing"))?;
            let incoming = self.incoming.take().ok_or_else(|| anyhow!("history stdout missing"))?;
            // Callers include ordinary Send tasks, which do not inherit the
            // master's LocalSet. Keep the SDK's local driver alive on its own
            // blocking thread while requests remain on the async worker.
            let runtime = tokio::runtime::Handle::current();
            let (ready, connection) = tokio::sync::oneshot::channel();
            self.io = Some(tokio::task::spawn_blocking(move || {
                runtime.block_on(LocalSet::new().run_until(async move {
                    let (connection, io) = conn::spawn_client(
                        acp::Client.builder().name("wta-history"),
                        conn::byte_streams(outgoing.compat_write(), incoming.compat()),
                    );
                    if let Err(connection) = ready.send(connection) {
                        connection.shutdown();
                    }
                    io.await
                }))
            }));
            self.conn = Some(connection.await.context("history ACP driver failed to start")?);
            let response = tokio::time::timeout(self.init_timeout, self.connection()?.initialize(
                acp::schema::v1::InitializeRequest::new(acp::schema::ProtocolVersion::V1)
                    .client_info(acp::schema::v1::Implementation::new("wta-history", env!("CARGO_PKG_VERSION"))),
            )).await.context("history ACP initialize timed out")??;
            if response.agent_capabilities.session_capabilities.list.is_none() {
                return Err(anyhow!("history ACP server does not support session/list"));
            }
            self.stderr.mark_initialized();
            self.initialized = true;
            tracing::info!(target: "master_history", agent_id = %target.agent_id, source = %target.source,
                pid = ?self.child.as_ref().and_then(|child| child.id()), "dedicated history ACP initialized");
            Ok(())
        }.await;
        if let Err(error) = result {
            for line in self.stderr.mark_failed() {
                tracing::warn!(target: "master_history", agent_id = %target.agent_id, "{line}");
            }
            return Err(error);
        }
        Ok(())
    }

    fn connection(&self) -> Result<&conn::ClientLink> {
        self.conn
            .as_ref()
            .ok_or_else(|| anyhow!("history connection missing"))
    }

    async fn stop(&mut self) -> Result<()> {
        #[cfg(test)]
        if self.reject_stop {
            return Err(anyhow!("fixture cannot confirm process exit"));
        }
        self.read_wsl_receipt().await?;
        if let Some(group) = &self.wsl_group {
            group.terminate().await?;
        }
        if let Some(job) = &self.job {
            job.terminate().await?;
        }
        if let Some(child) = &mut self.child {
            tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .context("history launcher exit timed out")?
                .context("wait for history launcher")?;
        }
        if let Some(conn) = &self.conn {
            conn.shutdown();
        }
        if let Some(io) = self.io.take() {
            io.abort();
            let _ = io.await;
        }
        Ok(())
    }

    fn ended(&mut self) -> Result<bool> {
        Ok(self.io.as_ref().is_some_and(|io| io.is_finished())
            || self
                .child
                .as_mut()
                .map(|child| child.try_wait())
                .transpose()?
                .flatten()
                .is_some())
    }

    #[cfg(test)]
    pub(super) fn mock(conn: conn::ClientLink) -> Self {
        Self {
            conn: Some(conn),
            child: None,
            job: None,
            wsl_group: None,
            wsl_bootstrap: None,
            incoming: None,
            receipt: Vec::new(),
            stderr: AgentStderrLog::new("history-fixture"),
            init_timeout: Duration::from_secs(15),
            initialized: true,
            io: None,
            reject_stop: false,
        }
    }
}

impl Drop for HistoryProcess {
    fn drop(&mut self) {
        if let Some(conn) = &self.conn {
            conn.shutdown();
        }
        if let Some(io) = &self.io {
            io.abort();
        }
    }
}

struct PendingList {
    started: Instant,
    checkpoint: Duration,
    response: tokio::sync::oneshot::Receiver<acp::Result<acp::schema::v1::ListSessionsResponse>>,
}

#[derive(Default)]
pub(super) struct HistoryQuery {
    pub process: Option<HistoryProcess>,
    pending: Option<PendingList>,
    recycling: bool,
    wait: AdaptiveWait,
}

impl HistoryQuery {
    #[cfg(test)]
    pub(super) fn mock(conn: conn::ClientLink) -> Self {
        Self {
            process: Some(HistoryProcess::mock(conn)),
            ..Default::default()
        }
    }

    #[cfg(test)]
    pub(super) fn expire_for_test(&mut self) {
        self.pending.as_mut().unwrap().started = Instant::now() - HARD_TIMEOUT;
    }

    #[cfg(test)]
    pub(super) fn reject_stop_for_test(&mut self, reject: bool) {
        self.process.as_mut().unwrap().reject_stop = reject;
    }

    pub fn in_flight(&self) -> bool {
        self.pending.is_some()
    }

    pub async fn stop(&mut self) -> Result<()> {
        self.pending = None;
        self.recycling = true;
        if let Some(process) = &mut self.process {
            process.stop().await?;
        }
        self.process = None;
        self.recycling = false;
        Ok(())
    }

    async fn recycle_timed_out(&mut self) -> Result<()> {
        tracing::warn!(target: "master_history",
            pid = ?self.process.as_ref().and_then(|process| process.child.as_ref()).and_then(|child| child.id()),
            "history request reached 300s hard deadline; recycling query process");
        self.wait.fast_successes = 0;
        self.stop().await
    }

    pub async fn poll(
        &mut self,
        target: Option<&HistoryTarget>,
        cancelled: &tokio_util::sync::CancellationToken,
    ) -> std::result::Result<Vec<acp::schema::v1::SessionInfo>, HistoryRefreshFailure> {
        match self.poll_inner(target, cancelled).await {
            Ok(result) => result,
            Err(error) => {
                self.wait.fast_successes = 0;
                tracing::warn!(target: "master_history",
                    pid = ?self.process.as_ref().and_then(|process| process.child.as_ref()).and_then(|child| child.id()),
                    error = %format!("{error:#}"), "dedicated history query failed");
                Err(HistoryRefreshFailure::Other)
            }
        }
    }

    async fn poll_inner(
        &mut self,
        target: Option<&HistoryTarget>,
        cancelled: &tokio_util::sync::CancellationToken,
    ) -> Result<std::result::Result<Vec<acp::schema::v1::SessionInfo>, HistoryRefreshFailure>> {
        if self.recycling {
            self.stop().await?;
        }
        if self
            .process
            .as_mut()
            .map(HistoryProcess::ended)
            .transpose()?
            .unwrap_or(false)
        {
            self.stop().await?;
            return Err(anyhow!("history ACP process or transport exited"));
        }
        if self.process.is_none() {
            self.process = Some(HistoryProcess::spawn(
                target.ok_or_else(|| anyhow!("history launch target missing"))?,
            )?);
        }
        if let Some(process) = self.process.as_mut().filter(|process| !process.initialized) {
            self.recycling = true;
            tokio::select! {
                biased;
                _ = cancelled.cancelled() => return Err(anyhow!("history startup retired")),
                result = process.initialize(target.ok_or_else(|| anyhow!("history launch target missing"))?) => result?,
            }
            self.recycling = false;
        }
        if self.pending.is_none() {
            let process = self
                .process
                .as_ref()
                .ok_or_else(|| anyhow!("history process missing"))?;
            let started = Instant::now();
            self.pending = Some(PendingList {
                started,
                checkpoint: self.wait.budget(),
                response: process.connection()?.start_list_sessions().await?,
            });
        }
        let pending = self
            .pending
            .as_mut()
            .ok_or_else(|| anyhow!("history request missing"))?;
        let hard_deadline = pending.started + HARD_TIMEOUT;
        if Instant::now() >= hard_deadline {
            self.recycle_timed_out().await?;
            return Ok(Err(HistoryRefreshFailure::Timeout));
        }
        let checkpoint = pending.started + pending.checkpoint;
        tokio::select! {
            biased;
            _ = cancelled.cancelled() => {
                self.stop().await?;
                Ok(Err(HistoryRefreshFailure::Other))
            }
            _ = tokio::time::sleep_until(hard_deadline) => {
                self.recycle_timed_out().await?;
                Ok(Err(HistoryRefreshFailure::Timeout))
            }
            response = &mut pending.response => {
                let elapsed = pending.started.elapsed();
                self.pending = None;
                let response = match response.context("history response transport ended")? {
                    Ok(response) => response,
                    Err(error) => {
                        if matches!(
                            crate::protocol::acp::failure::AgentFailure::from_acp_error(&error),
                            crate::protocol::acp::failure::AgentFailure::AuthRequired { .. }
                        ) {
                            // Pick up credentials established by the normal
                            // sign-in flow without authenticating this worker.
                            self.recycling = true;
                        }
                        return Err(error.into());
                    }
                };
                self.wait.success(elapsed);
                Ok(Ok(response.sessions))
            }
            _ = tokio::time::sleep_until(checkpoint) => {
                self.wait.timed_out();
                pending.checkpoint = if pending.checkpoint < Duration::from_secs(60) {
                    self.wait.budget()
                } else {
                    pending.checkpoint + Duration::from_secs(60)
                };
                tracing::warn!(target: "master_history", waited_secs = checkpoint.duration_since(pending.started).as_secs(),
                    pid = ?self.process.as_ref().and_then(|process| process.child.as_ref()).and_then(|child| child.id()),
                    next_wait_secs = self.wait.budget().as_secs(), "history soft timeout; retaining original request");
                Ok(Err(HistoryRefreshFailure::Timeout))
            }
        }
    }
}
