//! Disposable ACP history connection and retained single-flight requests.

use super::*;
use std::time::Duration;
use tokio::io::BufReader;
use tokio::time::Instant;

use crate::protocol::acp::process::AgentProcess;

const WAIT_STEPS: [u64; 5] = [5, 10, 20, 40, 60];
const HARD_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_INVENTORY_PAGES: usize = 1024;
const MAX_INVENTORY_ROWS: usize = 100_000;

#[derive(Clone)]
pub(super) struct HistoryTarget {
    pub startup_registry: Arc<crate::agent_lifecycle::StartupRegistry>,
    pub command: String,
    pub agent_id: String,
    pub source: crate::agent_source::AgentSource,
    pub provider: ProviderBinding,
}

impl HistoryTarget {
    fn admission_key(&self) -> String {
        crate::agent_lifecycle::agent_key(
            &self.command,
            Some(&self.agent_id),
            &self.source,
            &self.provider.admission_binding_key(),
            self.provider.is_model_scoped(),
        )
    }
}

async fn await_history_startup(
    startup: impl std::future::Future<Output = Result<HistoryProcess>>,
    cancelled: &tokio_util::sync::CancellationToken,
) -> Result<HistoryProcess> {
    tokio::select! {
        biased;
        _ = cancelled.cancelled() => Err(anyhow!("history startup retired")),
        result = startup => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    type PageReply =
        tokio::sync::oneshot::Sender<acp::Result<acp::schema::v1::ListSessionsResponse>>;

    fn controlled_query() -> (
        HistoryQuery,
        mpsc::UnboundedReceiver<(Option<String>, PageReply)>,
    ) {
        let (requests, receiver) = mpsc::unbounded_channel();
        let (client_pipe, agent_pipe) = tokio::io::duplex(8192);
        let (client_read, client_write) = tokio::io::split(client_pipe);
        let (agent_read, agent_write) = tokio::io::split(agent_pipe);
        let builder = acp::Agent
            .builder()
            .name("inventory-fixture")
            .on_receive_request(
                move |request: acp::schema::v1::ListSessionsRequest,
                      responder: acp::Responder<acp::schema::v1::ListSessionsResponse>,
                      _cx| {
                    let requests = requests.clone();
                    async move {
                        let (reply, response) = tokio::sync::oneshot::channel();
                        requests.send((request.cursor, reply)).unwrap();
                        match response.await {
                            Ok(Ok(page)) => responder.respond(page),
                            Ok(Err(error)) => responder.respond_with_error(error),
                            Err(_) => responder.respond_with_error(acp::Error::internal_error()),
                        }
                    }
                },
                acp::on_receive_request!(),
            );
        let (_, agent_io) = conn::spawn_agent(
            builder,
            conn::byte_streams(agent_write.compat_write(), agent_read.compat()),
        );
        let (client, client_io) = conn::spawn_client(
            acp::Client.builder().name("inventory-client"),
            conn::byte_streams(client_write.compat_write(), client_read.compat()),
        );
        tokio::task::spawn_local(async move {
            let _ = agent_io.await;
        });
        tokio::task::spawn_local(async move {
            let _ = client_io.await;
        });
        (HistoryQuery::mock(client), receiver)
    }

    fn page(id: &str, cursor: Option<&str>) -> acp::schema::v1::ListSessionsResponse {
        let mut row = acp::schema::v1::SessionInfo::new(id.to_string(), PathBuf::from("C:\\repo"));
        row.updated_at = Some("2026-10-16T08:00:00Z".into());
        let mut page = acp::schema::v1::ListSessionsResponse::new(vec![row]);
        page.next_cursor = cursor.map(str::to_string);
        page
    }

    #[tokio::test(start_paused = true)]
    async fn history_inventory_retains_later_page_across_soft_timeouts() {
        LocalSet::new()
            .run_until(async {
                let (mut query, mut requests) = controlled_query();
                let cancelled = tokio_util::sync::CancellationToken::new();
                let started = Instant::now();
                let mut poll = Box::pin(query.poll(None, &cancelled, true));
                let (cursor, first) = tokio::select! {
                    request = requests.recv() => request.unwrap(),
                    _ = &mut poll => panic!("missing first request"),
                };
                assert_eq!(cursor, None);
                first.send(Ok(page("old", Some("second")))).unwrap();
                let (cursor, second) = tokio::select! {
                    request = requests.recv() => request.unwrap(),
                    _ = &mut poll => panic!("missing second request"),
                };
                assert_eq!(cursor.as_deref(), Some("second"));
                tokio::time::advance(Duration::from_secs(5)).await;
                assert_eq!(poll.await.unwrap_err(), HistoryRefreshFailure::Timeout);
                assert_eq!(query.pending.as_ref().unwrap().started, started);
                assert_eq!(
                    query
                        .pending
                        .as_ref()
                        .unwrap()
                        .inventory
                        .as_ref()
                        .unwrap()
                        .sessions
                        .len(),
                    1
                );
                assert!(!query.last_response_complete());
                assert!(requests.try_recv().is_err());
                // A pending pane disappearing must not downgrade a retained full walk.
                let mut poll = Box::pin(query.poll(None, &cancelled, false));
                assert!(futures::poll!(&mut poll).is_pending());
                tokio::time::advance(Duration::from_secs(6)).await;
                assert_eq!(poll.await.unwrap_err(), HistoryRefreshFailure::Timeout);
                assert!(requests.try_recv().is_err());
                second.send(Ok(page("middle", Some("third")))).unwrap();
                let poll = query.poll(None, &cancelled, false);
                let reply = async {
                    let (cursor, third) = requests.recv().await.unwrap();
                    assert_eq!(cursor.as_deref(), Some("third"));
                    third.send(Ok(page("recent-native", None))).unwrap();
                };
                let listing = tokio::join!(poll, reply).0.unwrap();
                assert_eq!(
                    listing
                        .sessions
                        .iter()
                        .map(|row| row.session_id.to_string())
                        .collect::<Vec<_>>(),
                    ["old", "middle", "recent-native"]
                );
                assert!(listing
                    .sessions
                    .iter()
                    .all(|row| row.updated_at.as_deref() == Some("2026-10-16T08:00:00Z")));
                assert!(listing.next_cursor.is_none());
                assert!(query.last_response_complete());
                assert!(!query.in_flight());
                assert!(query.process.is_some());
                assert!(requests.try_recv().is_err());
            })
            .await;
    }

    #[tokio::test(start_paused = true)]
    async fn history_inventory_rejects_cursor_and_page_cycles() {
        LocalSet::new()
            .run_until(async {
                for (first_cursor, second_id, second_cursor) in [
                    ("second", "new", Some("second")),
                    ("second", "old", Some("third")),
                    ("", "unused", None),
                ] {
                    let (mut query, mut requests) = controlled_query();
                    let cancelled = tokio_util::sync::CancellationToken::new();
                    let poll = query.poll(None, &cancelled, true);
                    let reply = async {
                        let (_, first) = requests.recv().await.unwrap();
                        first.send(Ok(page("old", Some(first_cursor)))).unwrap();
                        if !first_cursor.is_empty() {
                            let (_, second) = requests.recv().await.unwrap();
                            second.send(Ok(page(second_id, second_cursor))).unwrap();
                        }
                    };
                    assert_eq!(
                        tokio::join!(poll, reply).0.unwrap_err(),
                        HistoryRefreshFailure::Other
                    );
                    assert!(!query.in_flight());
                    assert!(!query.last_response_complete());
                    assert!(requests.try_recv().is_err());
                }
            })
            .await;
    }

    #[tokio::test(start_paused = true)]
    async fn history_inventory_retirement_discards_pages_and_process() {
        LocalSet::new()
            .run_until(async {
                for hard_timeout in [false, true] {
                    let (mut query, mut requests) = controlled_query();
                    let cancelled = tokio_util::sync::CancellationToken::new();
                    let mut poll = Box::pin(query.poll(None, &cancelled, true));
                    let (_, first) = tokio::select! {
                        request = requests.recv() => request.unwrap(),
                        _ = &mut poll => panic!("missing first request"),
                    };
                    first.send(Ok(page("old", Some("second")))).unwrap();
                    let (_, late) = tokio::select! {
                        request = requests.recv() => request.unwrap(),
                        _ = &mut poll => panic!("missing second request"),
                    };
                    tokio::time::advance(Duration::from_secs(5)).await;
                    assert_eq!(poll.await.unwrap_err(), HistoryRefreshFailure::Timeout);
                    assert!(query.pending.as_ref().unwrap().inventory.is_some());
                    if hard_timeout {
                        // Absolute 300s deadline, not 300s after the last page.
                        tokio::time::advance(Duration::from_secs(295)).await;
                    } else {
                        cancelled.cancel();
                    }
                    assert_eq!(
                        query.poll(None, &cancelled, false).await.unwrap_err(),
                        if hard_timeout {
                            HistoryRefreshFailure::Timeout
                        } else {
                            HistoryRefreshFailure::Other
                        }
                    );
                    assert!(!query.in_flight());
                    assert!(query.process.is_none());
                    assert!(!query.last_response_complete());
                    let _ = late.send(Ok(page("late", None)));
                    assert!(requests.try_recv().is_err());
                }
            })
            .await;
    }

    #[tokio::test(start_paused = true)]
    async fn history_inventory_normal_discovery_keeps_first_page_cursor() {
        LocalSet::new()
            .run_until(async {
                let (mut query, mut requests) = controlled_query();
                let cancelled = tokio_util::sync::CancellationToken::new();
                let poll = query.poll(None, &cancelled, false);
                let reply = async {
                    let (cursor, first) = requests.recv().await.unwrap();
                    assert_eq!(cursor, None);
                    first.send(Ok(page("front", Some("second")))).unwrap();
                };
                let listing = tokio::join!(poll, reply).0.unwrap();
                assert_eq!(listing.sessions.len(), 1);
                assert_eq!(listing.next_cursor.as_deref(), Some("second"));
                assert!(!query.last_response_complete());
                assert!(!query.in_flight());
                assert!(requests.try_recv().is_err());
                assert_eq!(query.wait.budget(), Duration::from_secs(5));
            })
            .await;
    }

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

    #[tokio::test]
    async fn history_startup_cancellation_wins_before_ready_work() {
        let cancelled = tokio_util::sync::CancellationToken::new();
        cancelled.cancel();
        let polled = std::sync::atomic::AtomicBool::new(false);
        let result = await_history_startup(
            async {
                polled.store(true, std::sync::atomic::Ordering::Relaxed);
                Err(anyhow!("startup work must not run after retirement"))
            },
            &cancelled,
        )
        .await;
        assert!(!polled.load(std::sync::atomic::Ordering::Relaxed));
        assert!(result
            .err()
            .unwrap()
            .to_string()
            .contains("history startup retired"));
    }

    #[tokio::test(start_paused = true)]
    async fn history_startup_cancellation_releases_a_pending_refresh_gate() {
        let cancelled = tokio_util::sync::CancellationToken::new();
        let gate = tokio::sync::Mutex::new(());
        let (entered, ready) = tokio::sync::oneshot::channel();
        let work = async {
            let _guard = gate.lock().await;
            entered.send(()).unwrap();
            await_history_startup(std::future::pending::<Result<HistoryProcess>>(), &cancelled)
                .await
        };
        let retire = async {
            ready.await.unwrap();
            cancelled.cancel();
        };
        let started = Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(1), async {
            let (result, ()) = tokio::join!(work, retire);
            result
        })
        .await;
        assert!(
            result.is_ok(),
            "retirement left the refresh gate blocked on startup"
        );
        assert!(result
            .unwrap()
            .err()
            .unwrap()
            .to_string()
            .contains("history startup retired"));
        assert!(gate.try_lock().is_ok());
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test]
    async fn history_startup_preserves_uncancelled_failure() {
        let cancelled = tokio_util::sync::CancellationToken::new();
        let result = await_history_startup(
            std::future::ready(Err(anyhow!("dependency preparation failed"))),
            &cancelled,
        )
        .await;
        assert_eq!(
            result.err().unwrap().to_string(),
            "dependency preparation failed"
        );
    }

    fn fixture_target() -> HistoryTarget {
        HistoryTarget {
            startup_registry: Arc::default(),
            command: format!("wta-missing-history-{}.exe", uuid::Uuid::new_v4()),
            agent_id: "custom:history-ownership".into(),
            source: crate::agent_source::AgentSource::Host,
            provider: ProviderBinding::Native,
        }
    }

    fn fixture_process(target: &HistoryTarget) -> HistoryProcess {
        let (pipe, _peer) = tokio::io::duplex(1024);
        let (read, write) = tokio::io::split(pipe);
        let (connection, _io) = conn::spawn_client(
            acp::Client.builder().name("history-ownership-fixture"),
            conn::byte_streams(write.compat_write(), read.compat()),
        );
        let mut process = HistoryProcess::mock(connection);
        process.startup_registry = Arc::clone(&target.startup_registry);
        process.admission_key = target.admission_key();
        process
    }

    #[tokio::test]
    async fn receipt_cancellation_retains_child_and_admission_until_confirmed_stop() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let target = fixture_target();
                let key = target.admission_key();
                let mut process = fixture_process(&target);
                let mut lease = target.startup_registry.lock(&key).await.unwrap();
                lease.begin().unwrap();
                process.startup = Some(lease);
                process.initialized = false;
                process.conn = None;
                let mut command = tokio::process::Command::new("pwsh.exe");
                command
                    .args([
                        "-NoProfile",
                        "-Command",
                        "[Console]::In.ReadLine() | Out-Null",
                    ])
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .kill_on_drop(true);
                // The normal ownership reader waits for a receipt until stdin closes.
                // EOF avoids invoking WSL while exercising the real cancellable read.
                process.child = Some(
                    AgentProcess::spawn(&mut command, Some(("unused".into(), "receipt".into())))
                        .unwrap(),
                );
                let pid = process.child.as_ref().unwrap().id();
                let mut query = HistoryQuery {
                    process: Some(process),
                    ..Default::default()
                };
                let cancelled = tokio_util::sync::CancellationToken::new();
                {
                    let mut poll = std::pin::pin!(query.poll(Some(&target), &cancelled, false));
                    std::future::poll_fn(|cx| {
                        assert!(std::future::Future::poll(poll.as_mut(), cx).is_pending());
                        std::task::Poll::Ready(())
                    })
                    .await;
                    cancelled.cancel();
                    assert!(poll.await.is_err());
                }
                let process = query.process.as_mut().unwrap();
                assert_eq!(process.child.as_ref().unwrap().id(), pid);
                assert!(process.startup.is_some());
                assert!(query.recycling);
                {
                    let mut competing = std::pin::pin!(target.startup_registry.lock(&key));
                    std::future::poll_fn(|cx| {
                        assert!(std::future::Future::poll(competing.as_mut(), cx).is_pending());
                        std::task::Poll::Ready(())
                    })
                    .await;
                }
                {
                    let mut stop = std::pin::pin!(query.stop());
                    std::future::poll_fn(|cx| {
                        assert!(std::future::Future::poll(stop.as_mut(), cx).is_pending());
                        std::task::Poll::Ready(())
                    })
                    .await;
                }
                assert!(query.process.as_ref().unwrap().startup.is_some());
                drop(
                    query
                        .process
                        .as_mut()
                        .unwrap()
                        .child
                        .as_mut()
                        .unwrap()
                        .stdin
                        .take(),
                );
                tokio::time::timeout(Duration::from_secs(20), query.stop())
                    .await
                    .expect("retained receipt ownership permits confirmed cleanup")
                    .unwrap();
                assert!(query.process.is_none());
                target
                    .startup_registry
                    .lock(&key)
                    .await
                    .unwrap()
                    .begin()
                    .unwrap();
            })
            .await;
    }

    #[tokio::test]
    async fn post_initialize_stop_failure_quarantines_independent_admission() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let target = fixture_target();
                let key = target.admission_key();
                let mut process = fixture_process(&target);
                let mut startup = target.startup_registry.lock(&key).await.unwrap();
                startup.begin().unwrap();
                startup.succeeded().unwrap();
                drop(startup);
                assert!(process.initialized);
                assert!(process.startup.is_none());
                process.reject_stop = true;
                let error = process.stop().await.unwrap_err();
                assert_eq!(
                    error
                        .downcast_ref::<crate::agent_lifecycle::StartupFailure>()
                        .unwrap()
                        .phase,
                    crate::agent_lifecycle::StartupPhase::Cleanup,
                );
                let registry = Arc::clone(&target.startup_registry);
                let error = registry.lock(&key).await.unwrap().begin().unwrap_err();
                assert!(error.is::<crate::agent_lifecycle::StartupCleanupUnconfirmed>());
                assert!(registry.retry(&key).await.is_err());
            })
            .await;
    }

    #[tokio::test]
    async fn initialization_failure_reuses_its_lease_during_cleanup() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let target = fixture_target();
                let key = target.admission_key();
                let mut process = fixture_process(&target);
                let mut startup = target.startup_registry.lock(&key).await.unwrap();
                startup.begin().unwrap();
                process.startup = Some(startup);
                process.initialized = false;
                process.reject_stop = true;
                let error =
                    tokio::time::timeout(Duration::from_secs(1), process.initialize(&target))
                        .await
                        .expect("initialization cleanup must not reacquire its own lease")
                        .unwrap_err();
                assert_eq!(
                    error
                        .downcast_ref::<crate::agent_lifecycle::StartupFailure>()
                        .unwrap()
                        .phase,
                    crate::agent_lifecycle::StartupPhase::Cleanup,
                );
                assert!(target
                    .startup_registry
                    .lock(&key)
                    .await
                    .unwrap()
                    .begin()
                    .is_err());
            })
            .await;
    }

    #[tokio::test]
    async fn stopped_history_requires_a_new_registry_retry_epoch() {
        let target = fixture_target();
        let key = target.admission_key();
        let cancelled = tokio_util::sync::CancellationToken::new();
        let mut query = HistoryQuery::default();
        for _ in 0..crate::agent_lifecycle::MAX_STARTUP_ATTEMPTS {
            assert!(query.poll(Some(&target), &cancelled, false).await.is_err());
        }
        assert!(query.startup_stopped);
        assert!(query.poll(Some(&target), &cancelled, false).await.is_err());
        assert!(query.startup_stopped);
        target.startup_registry.retry(&key).await.unwrap();
        assert!(query.poll(Some(&target), &cancelled, false).await.is_err());
        assert!(!query.startup_stopped);
        assert_eq!(
            query.retry_epoch,
            target.startup_registry.retry_epoch(&key).await.unwrap()
        );
    }

    #[test]
    fn stale_failure_cannot_stop_a_new_retry_epoch() {
        let mut query = HistoryQuery::default();
        query.observe_startup_failure(0, 1);
        assert!(!query.startup_stopped);
        assert_eq!(query.retry_epoch, 1);
        query.observe_startup_failure(1, 1);
        assert!(query.startup_stopped);
        query.observe_startup_failure(0, 1);
        assert!(
            query.startup_stopped,
            "stale failure cannot change current state"
        );
        query.observe_startup_failure(1, 2);
        assert!(!query.startup_stopped);
        query.observe_startup_failure(0, 0);
        assert!(!query.startup_stopped);
        assert_eq!(query.retry_epoch, 2);
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
    startup_registry: Arc<crate::agent_lifecycle::StartupRegistry>,
    admission_key: String,
    startup: Option<crate::agent_lifecycle::StartupLease>,
    conn: Option<conn::ClientLink>,
    child: Option<AgentProcess>,
    incoming: Option<BufReader<tokio::process::ChildStdout>>,
    stderr: AgentStderrLog,
    init_timeout: Duration,
    initialized: bool,
    io: Option<tokio::task::JoinHandle<acp::Result<()>>>,
    #[cfg(test)]
    reject_stop: bool,
}

impl HistoryProcess {
    async fn spawn(target: &HistoryTarget) -> Result<Self> {
        let key = target.admission_key();
        let mut startup = target.startup_registry.lock(&key).await?;
        startup.begin()?;
        let prepared = crate::agent_lifecycle::providers::prepare(
            &target.command,
            Some(&target.agent_id),
            &target.source,
        )
        .await
        .context(crate::agent_lifecycle::StartupPhase::Preparation);
        match prepared {
            Ok(prepared) => startup.prepared(prepared),
            Err(error) => return Err(startup.failed(error)),
        }
        let spawned = crate::protocol::acp::spawn::spawn_history_agent_process(
            &target.command,
            &target.agent_id,
            &target.source,
            target.provider.spawn_selection(),
        )
        .context(crate::agent_lifecycle::StartupPhase::Spawn);
        let mut spawned = match spawned {
            Ok(spawned) => spawned,
            Err(error) => return Err(startup.failed(error)),
        };
        let stderr = AgentStderrLog::new(format!("history-{}", target.agent_id));
        if let Some(pipe) = spawned.child.stderr.take() {
            stderr.drain(pipe);
        }
        // No await after spawn: the query must own the child and admission
        // lease before receipt acquisition can be cancelled.
        Ok(Self {
            startup_registry: Arc::clone(&target.startup_registry),
            admission_key: key,
            startup: Some(startup),
            conn: None,
            child: Some(spawned.child),
            incoming: None,
            stderr,
            init_timeout: crate::agent_lifecycle::INITIALIZE_TIMEOUT,
            initialized: false,
            io: None,
            #[cfg(test)]
            reject_stop: false,
        })
    }

    async fn initialize(&mut self, target: &HistoryTarget) -> Result<()> {
        let result = async {
            let child = self.child.as_mut().ok_or_else(|| anyhow!("history child missing"))?;
            if self.incoming.is_none() {
                self.incoming = Some(child.take_stdout().await?);
            }
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
            if let Some(startup) = &mut self.startup {
                startup.succeeded()?;
            }
            self.startup = None;
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
            let lines = self.stderr.mark_failed();
            for line in &lines {
                tracing::warn!(target: "master_history", agent_id = %target.agent_id, "{line}");
            }
            // Keep an initialization lease in self across cleanup awaits.
            // Reacquiring it here would deadlock, and a local would be lost
            // if retirement cancels cleanup.
            let cleanup = self.cleanup().await;
            let error = crate::agent_lifecycle::StartupDetails::attach(
                cleanup.err().unwrap_or(error),
                &lines,
            );
            return Err(match self.startup.take() {
                Some(startup) => startup.failed(error),
                None => error,
            });
        }
        Ok(())
    }

    fn connection(&self) -> Result<&conn::ClientLink> {
        self.conn
            .as_ref()
            .ok_or_else(|| anyhow!("history connection missing"))
    }

    async fn stop(&mut self) -> Result<()> {
        let result = self.cleanup().await;
        let startup = self.startup.take();
        match result {
            Ok(()) => Ok(()),
            Err(error) => Err(match startup {
                Some(startup) => startup.failed(error),
                None => error,
            }),
        }
    }

    async fn cleanup(&mut self) -> Result<()> {
        if self.startup.is_none() {
            self.startup = Some(self.startup_registry.lock(&self.admission_key).await?);
        }
        let result: Result<()> = async {
            #[cfg(test)]
            if self.reject_stop {
                return Err(anyhow!("fixture cannot confirm process exit"));
            }
            if let Some(child) = &mut self.child {
                child.terminate().await?;
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
        .await;
        result
            .context(crate::agent_lifecycle::StartupPhase::Cleanup)
            .context(crate::agent_lifecycle::StartupCleanupUnconfirmed)
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
            startup_registry: Arc::default(),
            admission_key: "history-fixture".into(),
            startup: None,
            conn: Some(conn),
            child: None,
            incoming: None,
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
    complete_inventory: bool,
    inventory: Option<acp::schema::v1::ListSessionsResponse>,
    cursors: HashSet<String>,
    page_ids: HashSet<Vec<String>>,
    pages: usize,
}

impl PendingList {
    fn accumulate(
        &mut self,
        mut page: acp::schema::v1::ListSessionsResponse,
    ) -> Result<Option<String>> {
        self.pages += 1;
        let rows = self
            .inventory
            .as_ref()
            .map_or(0, |response| response.sessions.len());
        if self.pages > MAX_INVENTORY_PAGES || rows + page.sessions.len() > MAX_INVENTORY_ROWS {
            return Err(anyhow!("history inventory pagination limit exceeded"));
        }
        // Detect servers cycling pages even when they keep inventing cursors.
        let mut ids: Vec<_> = page
            .sessions
            .iter()
            .map(|row| row.session_id.to_string())
            .collect();
        ids.sort_unstable();
        if !self.page_ids.insert(ids) {
            return Err(anyhow!("history inventory page repeated"));
        }
        let next = page.next_cursor.take();
        if let Some(cursor) = &next {
            if cursor.is_empty() || !self.cursors.insert(cursor.clone()) {
                return Err(anyhow!("history inventory cursor empty or repeated"));
            }
        }
        if let Some(inventory) = &mut self.inventory {
            inventory.sessions.extend(page.sessions);
        } else {
            self.inventory = Some(page);
        }
        Ok(next)
    }
}

#[derive(Default)]
pub(super) struct HistoryQuery {
    pub process: Option<HistoryProcess>,
    pending: Option<PendingList>,
    recycling: bool,
    wait: AdaptiveWait,
    startup_stopped: bool,
    retry_epoch: u64,
    last_response_complete: bool,
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

    fn observe_startup_failure(&mut self, failed_epoch: u64, registry_epoch: u64) {
        let retried = registry_epoch > self.retry_epoch;
        self.retry_epoch = self.retry_epoch.max(registry_epoch);
        if failed_epoch < self.retry_epoch {
            if retried {
                self.startup_stopped = false;
            }
            return;
        }
        self.startup_stopped = true;
        self.retry_epoch = self.retry_epoch.max(failed_epoch);
    }

    pub fn last_response_complete(&self) -> bool {
        self.last_response_complete
    }

    pub async fn stop(&mut self) -> Result<()> {
        self.pending = None;
        self.last_response_complete = false;
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
        complete_inventory: bool,
    ) -> std::result::Result<acp::schema::v1::ListSessionsResponse, HistoryRefreshFailure> {
        match self.poll_inner(target, cancelled, complete_inventory).await {
            Ok(result) => result,
            Err(error) => {
                if error.is::<crate::agent_lifecycle::StartupLimitReached>()
                    || error.is::<crate::agent_lifecycle::StartupCleanupUnconfirmed>()
                {
                    let failed_epoch = error
                        .downcast_ref::<crate::agent_lifecycle::StartupFailure>()
                        .map(|failure| failure.retry_epoch)
                        .unwrap_or(self.retry_epoch);
                    let registry_epoch = if let Some(target) = target {
                        match target
                            .startup_registry
                            .retry_epoch(&target.admission_key())
                            .await
                        {
                            Ok(epoch) => epoch,
                            Err(epoch_error) => {
                                tracing::warn!(target: "master_history", error = %epoch_error,
                                    "could not read history startup retry epoch");
                                self.retry_epoch
                            }
                        }
                    } else {
                        self.retry_epoch
                    };
                    self.observe_startup_failure(failed_epoch, registry_epoch);
                }
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
        complete_inventory: bool,
    ) -> Result<std::result::Result<acp::schema::v1::ListSessionsResponse, HistoryRefreshFailure>>
    {
        if self.startup_stopped {
            let Some(target) = target else {
                return Ok(Err(HistoryRefreshFailure::Other));
            };
            let epoch = target
                .startup_registry
                .retry_epoch(&target.admission_key())
                .await?;
            if epoch <= self.retry_epoch {
                return Ok(Err(HistoryRefreshFailure::Other));
            }
            self.retry_epoch = epoch;
            self.startup_stopped = false;
        }
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
            self.process = Some(
                await_history_startup(
                    HistoryProcess::spawn(
                        target.ok_or_else(|| anyhow!("history launch target missing"))?,
                    ),
                    cancelled,
                )
                .await?,
            );
            if let Some(startup) = self
                .process
                .as_ref()
                .and_then(|process| process.startup.as_ref())
            {
                self.retry_epoch = self.retry_epoch.max(startup.retry_epoch());
            }
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
            self.last_response_complete = false;
            let process = self
                .process
                .as_ref()
                .ok_or_else(|| anyhow!("history process missing"))?;
            let started = Instant::now();
            self.pending = Some(PendingList {
                started,
                checkpoint: self.wait.budget(),
                response: process.connection()?.start_list_sessions().await?,
                complete_inventory,
                inventory: None,
                cursors: HashSet::new(),
                page_ids: HashSet::new(),
                pages: 0,
            });
        }
        loop {
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
            let response = tokio::select! {
                biased;
                _ = cancelled.cancelled() => {
                    self.stop().await?;
                    return Ok(Err(HistoryRefreshFailure::Other));
                }
                _ = tokio::time::sleep_until(hard_deadline) => {
                    self.recycle_timed_out().await?;
                    return Ok(Err(HistoryRefreshFailure::Timeout));
                }
                response = &mut pending.response => {
                    response
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
                    return Ok(Err(HistoryRefreshFailure::Timeout));
                }
            };
            let mut pending = self
                .pending
                .take()
                .ok_or_else(|| anyhow!("history request missing"))?;
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
            let response = if pending.complete_inventory {
                match pending.accumulate(response)? {
                    Some(cursor) => {
                        pending.response = self
                            .process
                            .as_ref()
                            .ok_or_else(|| anyhow!("history process missing"))?
                            .connection()?
                            .start_list_sessions_page(
                                acp::schema::v1::ListSessionsRequest::new().cursor(cursor),
                            )
                            .await?;
                        // Preserve the original start, soft checkpoint and all
                        // collected pages across calls; never restart a page.
                        self.pending = Some(pending);
                        continue;
                    }
                    None => pending
                        .inventory
                        .take()
                        .ok_or_else(|| anyhow!("history inventory missing"))?,
                }
            } else {
                response
            };
            self.wait.success(pending.started.elapsed());
            self.last_response_complete = response.next_cursor.is_none();
            return Ok(Ok(response));
        }
    }

    pub async fn list_cwd_page(
        &mut self,
        cursor: String,
        deadline: Instant,
        cancelled: &tokio_util::sync::CancellationToken,
    ) -> Option<acp::schema::v1::ListSessionsResponse> {
        let request = acp::schema::v1::ListSessionsRequest::new().cursor(cursor);
        let result = match self.process.as_ref().map(HistoryProcess::connection) {
            Some(Ok(connection)) => {
                tokio::select! {
                    biased;
                    _ = cancelled.cancelled() => Err(anyhow!("history cwd page cancelled")),
                    response = tokio::time::timeout_at(deadline, connection.list_sessions(request)) => {
                        response.context("history cwd page timed out").and_then(|response| response.map_err(anyhow::Error::from))
                    }
                }
            }
            Some(Err(error)) => Err(error),
            None => Err(anyhow!("history cwd process missing")),
        };
        match result {
            Ok(response) => Some(response),
            Err(error) => {
                tracing::warn!(target: "master_history", %error, "dedicated history cwd pagination incomplete");
                if let Err(stop_error) = self.stop().await {
                    tracing::error!(target: "master_history", %stop_error, "incomplete history query remains quarantined");
                }
                None
            }
        }
    }
}
