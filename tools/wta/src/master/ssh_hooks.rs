//! Master-owned ordinary-SSH routes and independent, noninteractive hook channels.
//! V3 updates SSH source registries directly and publishes ACP source-change
//! notifications; it does not rebroadcast COM `agent_event` notifications.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use agent_client_protocol as acp;
use anyhow::{anyhow, ensure, Context, Result};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{watch, Mutex, Notify};
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use super::MasterStateInner;
use crate::ssh_hook_protocol::{
    Registration, Request, Response, RouteId, TrackingStatus, ROUTE_LEASE,
};
use crate::ssh_sessions::SshTarget;

const MAX_ROUTES: usize = 128;
const MAX_TARGETS: usize = 32;
const MAX_REVOKED: usize = 1024;
const MAX_CONTROL_LINE: usize = 16 * 1024;
const SETUP_TIMEOUT: Duration = Duration::from_secs(45);
const REAP_TIMEOUT: Duration = Duration::from_secs(3);

struct Route {
    registration: Registration,
    renewed: Instant,
}

#[derive(Default)]
struct Routes {
    active: HashMap<RouteId, Route>,
    revoked: VecDeque<RouteId>,
}

impl Routes {
    fn expire(&mut self, now: Instant) {
        self.active
            .retain(|_, route| now.duration_since(route.renewed) < ROUTE_LEASE);
    }

    fn register(&mut self, registration: Registration, now: Instant) -> Result<()> {
        self.expire(now);
        ensure!(
            !self.revoked.contains(&registration.route),
            "SSH connection route has been revoked"
        );
        if let Some(existing) = self.active.get_mut(&registration.route) {
            ensure!(
                existing.registration == registration,
                "SSH route identity cannot change"
            );
            existing.renewed = now;
            return Ok(());
        }
        ensure!(
            !self
                .active
                .values()
                .any(|route| route.registration.pane_id == registration.pane_id),
            "A live SSH route already owns this native pane"
        );
        ensure!(
            self.active.len() < MAX_ROUTES,
            "SSH connection route limit reached"
        );
        self.active.insert(
            registration.route,
            Route {
                registration,
                renewed: now,
            },
        );
        Ok(())
    }

    fn revoke(&mut self, registration: &Registration) -> Result<bool> {
        if let Some(existing) = self.active.get(&registration.route) {
            ensure!(
                existing.registration == *registration,
                "SSH route revocation identity mismatch"
            );
        }
        let removed = self.active.remove(&registration.route).is_some();
        self.remember_revocation(registration.route);
        Ok(removed)
    }

    fn remember_revocation(&mut self, route: RouteId) {
        if !self.revoked.contains(&route) {
            if self.revoked.len() == MAX_REVOKED {
                self.revoked.pop_front();
            }
            self.revoked.push_back(route);
        }
    }
}

struct Channel {
    cancellation: CancellationToken,
    reconcile: Arc<Notify>,
    reconciling: Arc<AtomicBool>,
    status: watch::Receiver<TrackingStatus>,
    task: AbortOnDropHandle<()>,
}

pub(super) struct Service {
    enabled: AtomicBool,
    routes: Mutex<Routes>,
    channels: Mutex<HashMap<SshTarget, Channel>>,
}

impl Default for Service {
    fn default() -> Self {
        Self::new(false)
    }
}

impl Service {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            routes: Mutex::new(Routes::default()),
            channels: Mutex::new(HashMap::new()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    async fn active_route(&self, target: &SshTarget, route: RouteId) -> Option<Registration> {
        let mut routes = self.routes.lock().await;
        routes.expire(Instant::now());
        routes
            .active
            .get(&route)
            .filter(|route| route.registration.target == *target && !route.registration.no_hooks)
            .map(|route| route.registration.clone())
    }

    async fn has_target(&self, target: &SshTarget) -> bool {
        let mut routes = self.routes.lock().await;
        routes.expire(Instant::now());
        routes
            .active
            .values()
            .any(|route| !route.registration.no_hooks && route.registration.target == *target)
    }

    async fn stop_unused(&self) {
        let mut channels = self.channels.lock().await;
        channels.retain(|_, channel| !channel.task.is_finished());
        for (target, channel) in channels.iter() {
            if !self.enabled() || !self.has_target(target).await {
                channel.cancellation.cancel();
            }
        }
    }
}

fn rpc_error(error: anyhow::Error) -> acp::Error {
    acp::Error::invalid_request().data(serde_json::json!({ "message": format!("{error:#}") }))
}

fn permitted_clis(state: &MasterStateInner) -> Vec<&'static str> {
    ["copilot", "claude", "codex", "gemini", "opencode"]
        .into_iter()
        .filter(|cli| {
            state
                .allowed_agent_ids
                .as_ref()
                .is_none_or(|allowed| allowed.contains(*cli))
        })
        .collect()
}

pub(super) async fn handle(
    state: &Arc<MasterStateInner>,
    request: Request,
) -> acp::Result<acp::schema::v1::ExtResponse> {
    let state = Arc::clone(state);
    let response = tokio::task::spawn_local(async move {
        match request {
            Request::Register { mut registration } => {
                registration.validate().map_err(rpc_error)?;
                let wt = state
                    .wt
                    .as_ref()
                    .ok_or_else(|| rpc_error(anyhow!("Native SSH pane validation unavailable")))?;
                let status = wt
                    .request(
                        "get_process_status",
                        serde_json::json!({
                            "session_id": registration.pane_id,
                        }),
                    )
                    .await
                    .map_err(rpc_error)?;
                if status.get("state").and_then(|value| value.as_str()) != Some("running")
                    || status.get("pid").and_then(|value| value.as_u64())
                        != Some(u64::from(registration.wrapper_pid))
                {
                    return Err(rpc_error(anyhow!(
                        "Native SSH pane does not belong to this live wrapper"
                    )));
                }
                state
                    .ssh_hooks
                    .routes
                    .lock()
                    .await
                    .register(registration.clone(), Instant::now())
                    .map_err(rpc_error)?;
                let status = if registration.no_hooks {
                    TrackingStatus::OptedOut
                } else {
                    ensure_target(&state, &registration.target).await
                };
                Ok(Response { status })
            }
            Request::Revoke { mut registration } => {
                registration.validate().map_err(rpc_error)?;
                let removed = state
                    .ssh_hooks
                    .routes
                    .lock()
                    .await
                    .revoke(&registration)
                    .map_err(rpc_error)?;
                if removed {
                    super::ssh_sessions::pane_closed(&state, &registration.pane_id).await;
                }
                state.ssh_hooks.stop_unused().await;
                Ok(Response {
                    status: TrackingStatus::Disabled,
                })
            }
            Request::Ensure => {
                if !state.ssh_hooks.enabled() {
                    return Ok(Response {
                        status: TrackingStatus::Disabled,
                    });
                }
                let status = reconcile_registered(&state).await;
                Ok(Response { status })
            }
        }
    })
    .await
    .map_err(|_| rpc_error(anyhow!("SSH route request task failed")))??;
    let raw =
        serde_json::value::to_raw_value(&response).map_err(|error| rpc_error(error.into()))?;
    Ok(acp::schema::v1::ExtResponse::new(raw.into()))
}

async fn ensure_target(state: &Arc<MasterStateInner>, target: &SshTarget) -> TrackingStatus {
    if !state.ssh_hooks.enabled() {
        return TrackingStatus::Disabled;
    }
    if permitted_clis(state).is_empty() {
        return TrackingStatus::Unavailable {
            reason: "All remote hook providers are blocked by policy".to_owned(),
        };
    }
    if !state.ssh_hooks.has_target(target).await {
        return TrackingStatus::Disabled;
    }
    let mut channels = state.ssh_hooks.channels.lock().await;
    channels.retain(|_, channel| !channel.task.is_finished());
    if let Some(channel) = channels.get(target) {
        if channel.cancellation.is_cancelled() {
            return TrackingStatus::Connecting;
        }
        return channel.status.borrow().clone();
    }
    if channels.len() >= MAX_TARGETS {
        return TrackingStatus::Unavailable {
            reason: "SSH background target limit reached".to_owned(),
        };
    }
    let cancellation = CancellationToken::new();
    let reconcile = Arc::new(Notify::new());
    let reconciling = Arc::new(AtomicBool::new(true));
    let (status, receiver) = watch::channel(TrackingStatus::Connecting);
    let task = tokio::task::spawn_local(channel_loop(
        Arc::downgrade(state),
        target.clone(),
        cancellation.clone(),
        status,
        Arc::clone(&reconcile),
        Arc::clone(&reconciling),
    ));
    channels.insert(
        target.clone(),
        Channel {
            cancellation,
            reconcile,
            reconciling,
            status: receiver,
            task: AbortOnDropHandle::new(task),
        },
    );
    TrackingStatus::Connecting
}

pub(super) async fn configure(state: &Arc<MasterStateInner>, enabled: bool) {
    state.ssh_hooks.enabled.store(enabled, Ordering::Release);
    if enabled {
        reconcile_registered(state).await;
    } else {
        state.ssh_hooks.stop_unused().await;
    }
}

async fn reconcile_registered(state: &Arc<MasterStateInner>) -> TrackingStatus {
    let targets: std::collections::HashSet<_> = {
        let mut routes = state.ssh_hooks.routes.lock().await;
        routes.expire(Instant::now());
        routes
            .active
            .values()
            .filter(|route| !route.registration.no_hooks)
            .map(|route| route.registration.target.clone())
            .collect()
    };
    let mut status = TrackingStatus::Disabled;
    for target in targets {
        let mut next = ensure_target(state, &target).await;
        if let Some(channel) = state.ssh_hooks.channels.lock().await.get(&target) {
            // A window broadcast and its external hooks-install process may
            // request the same work. One in-flight reconcile covers that burst.
            if !channel.reconciling.swap(true, Ordering::AcqRel) {
                channel.reconcile.notify_one();
                next = TrackingStatus::Connecting;
            }
        }
        let priority = |value: &TrackingStatus| match value {
            TrackingStatus::Unavailable { .. } => 4,
            TrackingStatus::Partial { .. } => 3,
            TrackingStatus::Connecting => 2,
            TrackingStatus::Ready => 1,
            TrackingStatus::Disabled | TrackingStatus::OptedOut => 0,
        };
        if priority(&next) > priority(&status) {
            status = next;
        }
    }
    status
}

pub(super) async fn pane_closed(state: &MasterStateInner, pane: &str) {
    let pane = crate::agent_sessions::pane_key(pane);
    let mut routes = state.ssh_hooks.routes.lock().await;
    let removed: Vec<_> = routes
        .active
        .iter()
        .filter(|(_, route)| route.registration.pane_id == pane)
        .map(|(id, _)| *id)
        .collect();
    for route in removed {
        routes.active.remove(&route);
        routes.remember_revocation(route);
    }
    drop(routes);
    state.ssh_hooks.stop_unused().await;
}

async fn channel_loop(
    state: Weak<MasterStateInner>,
    target: SshTarget,
    cancellation: CancellationToken,
    status: watch::Sender<TrackingStatus>,
    reconcile: Arc<Notify>,
    reconciling: Arc<AtomicBool>,
) {
    let mut backoff = Duration::from_secs(2);
    let mut assembler = crate::ssh_hook_protocol::Reassembler::default();
    loop {
        let Some(current) = state.upgrade() else {
            return;
        };
        if cancellation.is_cancelled() || !current.ssh_hooks.has_target(&target).await {
            return;
        }
        let allowed = permitted_clis(&current);
        let enabled = current.ssh_hooks.enabled();
        drop(current);
        if !enabled || allowed.is_empty() {
            status.send_replace(TrackingStatus::Disabled);
            return;
        }
        status.send_replace(TrackingStatus::Connecting);
        let result = run_channel(
            &state,
            &target,
            &allowed,
            &cancellation,
            &status,
            &mut assembler,
            &reconcile,
            &reconciling,
        )
        .await;
        assembler.reset_connection();
        if cancellation.is_cancelled() {
            return;
        }
        reconciling.store(false, Ordering::Release);
        if result.is_ok() {
            backoff = Duration::from_secs(2);
            continue;
        }
        if let Err(error) = result {
            // Errors here contain only local/static diagnostics; never stderr
            // from the remote process, hook JSON, or arbitrary control output.
            tracing::warn!(target: "ssh_hooks", reason = %error, "Background SSH hook channel unavailable");
            status.send_replace(TrackingStatus::Unavailable {
                reason: error.to_string(),
            });
        }
        tokio::select! {
            _ = cancellation.cancelled() => return,
            _ = reconcile.notified() => {},
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

fn bootstrap_command(installer_bytes: usize, hook_bytes: usize, clis: &[&str]) -> Result<String> {
    ensure!(
        (1..=256 * 1024).contains(&installer_bytes) && (1..=256 * 1024).contains(&hook_bytes),
        "Remote hook upload asset size exceeds limit"
    );
    ensure!(
        !clis.is_empty()
            && clis.len() <= 5
            && clis
                .iter()
                .all(|cli| crate::agent_registry::is_known_id(cli)),
        "Invalid remote hook provider selection"
    );
    let unique: std::collections::HashSet<_> = clis.iter().collect();
    ensure!(
        unique.len() == clis.len(),
        "Duplicate remote hook provider selection"
    );
    // Persistent namespace ownership belongs to the installer. Until it runs,
    // write only these two files inside an atomically-created private directory.
    let script = r#"set -eu
umask 077
case "$1" in ''|*[!0-9]*) exit 1;; esac
case "$2" in ''|*[!0-9]*) exit 1;; esac
[ "$1" -gt 0 ] && [ "$1" -le 262144 ] || exit 1
[ "$2" -gt 0 ] && [ "$2" -le 262144 ] || exit 1
it_stage=$(mktemp -d -- "${TMPDIR:-/tmp}/it-ssh-upload.XXXXXXXXXXXX")
trap 'rm -f -- "$it_stage/install-remote-hooks.sh" "$it_stage/it-agent-hook.sh"; rmdir -- "$it_stage"' 0
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
timeout --kill-after=1 20 dd bs=1 count="$1" of="$it_stage/install-remote-hooks.sh" 2>/dev/null
[ "$(wc -c < "$it_stage/install-remote-hooks.sh")" -eq "$1" ] || exit 1
timeout --kill-after=1 20 dd bs=1 count="$2" of="$it_stage/it-agent-hook.sh" 2>/dev/null
[ "$(wc -c < "$it_stage/it-agent-hook.sh")" -eq "$2" ] || exit 1
it_home=$(getent passwd "$(id -u)" | cut -d: -f6)
sh -lc 'exec sh "$@"' it-ssh-setup "$it_stage/install-remote-hooks.sh" --hook-source "$it_stage/it-agent-hook.sh" --login-home "$it_home" --socket "$it_home/.intelligent-terminal/run/tmux-hooks.sock" --session it-hooks --allowed-clis "$3" --attach-control
"#;
    Ok(format!(
        "sh -c {} it-ssh-upload {installer_bytes} {hook_bytes} {}",
        crate::coordinator::sh_quote(script),
        crate::coordinator::sh_quote(&clis.join(","))
    ))
}

async fn read_line_bounded<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> Result<bool> {
    // Keep partial bytes across select! cancellation by maintenance or setup
    // signals. The caller takes the buffer only after a complete line.
    loop {
        let buffer = reader
            .fill_buf()
            .await
            .context("Read background SSH control stream")?;
        if buffer.is_empty() {
            ensure!(line.is_empty(), "Incomplete background SSH control line");
            return Ok(false);
        }
        let length = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |index| index + 1);
        ensure!(
            line.len() + length <= MAX_CONTROL_LINE,
            "Background SSH control line exceeds limit"
        );
        let complete = buffer[length - 1] == b'\n';
        line.extend_from_slice(&buffer[..length]);
        reader.consume(length);
        if complete {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(true);
        }
    }
}

async fn run_channel(
    state: &Weak<MasterStateInner>,
    target: &SshTarget,
    clis: &[&str],
    cancellation: &CancellationToken,
    status: &watch::Sender<TrackingStatus>,
    assembler: &mut crate::ssh_hook_protocol::Reassembler,
    reconcile: &Notify,
    reconciling: &AtomicBool,
) -> Result<()> {
    let (installer, hook) =
        tokio::task::spawn_blocking(crate::agent_hooks_installer::remote_hook_assets)
            .await
            .context("Read remote hook assets task")?
            .map_err(|_| {
                anyhow!("Current on-disk remote hook bundle is unavailable or incomplete")
            })?;
    let Some(current) = state.upgrade() else {
        return Ok(());
    };
    if cancellation.is_cancelled()
        || !current.ssh_hooks.enabled()
        || !current.ssh_hooks.has_target(target).await
    {
        return Ok(());
    }
    drop(current);
    let mut args = crate::ssh_sessions::ssh_arguments(target, false, "");
    let command = args.last_mut().context("Missing SSH bootstrap command")?;
    *command = bootstrap_command(installer.len(), hook.len(), clis)?;
    let mut command = tokio::process::Command::new(crate::ssh_sessions::system_ssh_executable()?);
    command
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    crate::ssh_sessions::configure_ssh_environment(&mut command, std::env::vars_os());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let mut child = command
        .spawn()
        .context("Start background Windows OpenSSH")?;
    let mut input = child
        .stdin
        .take()
        .context("Background SSH stdin unavailable")?;
    let output = child
        .stdout
        .take()
        .context("Background SSH stdout unavailable")?;
    let mut errors = child
        .stderr
        .take()
        .context("Background SSH stderr unavailable")?;
    // Drain without retaining or logging untrusted stderr; SSH/installer errors
    // can contain secrets or arbitrary terminal output from user startup files.
    let _stderr = AbortOnDropHandle::new(tokio::task::spawn_local(async move {
        let mut sink = tokio::io::sink();
        if tokio::io::copy(&mut errors, &mut sink).await.is_err() {
            tracing::debug!(target: "ssh_hooks", "Background SSH diagnostic stream closed with an I/O error");
        }
    }));
    let result = async {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Ok(()),
            _ = reconcile.notified() => return Ok(()),
            uploaded = tokio::time::timeout(SETUP_TIMEOUT, async {
                input.write_all(&installer).await?;
                input.write_all(&hook).await?;
                input.flush().await
            }) => {
                uploaded.context("Timed out uploading managed SSH hooks")?
                    .context("Upload managed SSH hook assets")?;
            }
        }
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        input.write_all(format!("display-message -p 'IT_HOOK_READY/3 {nonce} #{{@it-ssh-hooks-protocol}} #{{@it-ssh-hooks-installed-clis}} #{{@it-ssh-hooks-unavailable-clis}}'\n").as_bytes()).await?;
        input.flush().await?;
        let mut readiness = Readiness::new(&nonce);
        let deadline = tokio::time::Instant::now() + SETUP_TIMEOUT;
        let mut ready = false;
        let mut reader = BufReader::new(output);
        let mut line = Vec::new();
        let mut maintenance = tokio::time::interval(Duration::from_secs(10));
        let mut rejection_reported = false;
        loop {
            tokio::select! {
                _ = cancellation.cancelled() => return Ok(()),
                _ = reconcile.notified() => return Ok(()),
                _ = tokio::time::sleep_until(deadline), if !ready => {
                    return Err(anyhow!("Remote hooks did not become ready; trusted noninteractive SSH and supported CLI runtimes are required"));
                }
                _ = maintenance.tick() => {
                    let Some(current) = state.upgrade() else { return Ok(()) };
                    if !current.ssh_hooks.has_target(target).await { return Ok(()) }
                    assembler.expire(Instant::now());
                }
                read = read_line_bounded(&mut reader, &mut line) => {
                    if !read? {
                        return Err(anyhow!("Background SSH hook channel disconnected; foreground SSH is unchanged"));
                    }
                    let complete_line = std::mem::take(&mut line);
                    let text = std::str::from_utf8(&complete_line).context("Invalid UTF-8 in SSH control stream")?;
                    if let Some(tracking) = readiness.observe(text, clis)? {
                        ready = true;
                        status.send_replace(tracking);
                        reconciling.store(false, Ordering::Release);
                        continue;
                    }
                    let Some(frame) = readiness.hook_frame(text) else { continue };
                    let frame = match crate::ssh_hook_protocol::parse_frame(frame) {
                        Ok(frame) => frame,
                        Err(_) => {
                            report_rejection(&mut rejection_reported);
                            continue;
                        }
                    };
                    if !clis.contains(&frame.cli) {
                        report_rejection(&mut rejection_reported);
                        continue;
                    }
                    let Some(current) = state.upgrade() else { return Ok(()) };
                    let Some(registration) = current.ssh_hooks.active_route(target, frame.route).await else {
                        assembler.remove_route(frame.route);
                        continue;
                    };
                    match assembler.push(frame, Instant::now()) {
                        Ok(Some(event)) => {
                            if apply_routed_hook(&current, target, registration.route, event).await.is_err() {
                                report_rejection(&mut rejection_reported);
                            }
                        }
                        Ok(None) => {}
                        Err(_) => report_rejection(&mut rejection_reported),
                    }
                }
            }
        }
    }.await;
    drop(input);
    let cleanup = tokio::time::timeout(REAP_TIMEOUT, child.wait()).await;
    match cleanup {
        Ok(waited) => {
            waited.context("Reap the background SSH child")?;
        }
        Err(_) => {
            tokio::time::timeout(REAP_TIMEOUT, child.kill())
                .await
                .context("Timed out reaping the background SSH child")?
                .context("Reap the background SSH child")?;
        }
    }
    result
}

struct Readiness {
    marker: String,
    command: Option<String>,
    pending: Option<(String, TrackingStatus)>,
    completed: Option<TrackingStatus>,
    attached: bool,
    reported: bool,
}

impl Readiness {
    fn new(nonce: &str) -> Self {
        Self {
            marker: format!("IT_HOOK_READY/3 {nonce} "),
            command: None,
            pending: None,
            completed: None,
            attached: false,
            reported: false,
        }
    }

    fn hook_frame<'a>(&self, line: &'a str) -> Option<&'a str> {
        // Once natively attached, route-authorized hook notifications may
        // interleave with the advisory nonce reply. Do not discard those births.
        self.attached.then(|| control_hook_frame(line)).flatten()
    }

    fn observe(&mut self, line: &str, allowed: &[&str]) -> Result<Option<TrackingStatus>> {
        if self.reported {
            return Ok(None);
        }
        if let Some(session) = line.strip_prefix("%session-changed ") {
            let fields: Vec<_> = session.split(' ').collect();
            self.attached = fields.len() == 2
                && fields[1] == "it-hooks"
                && fields[0].strip_prefix('$').is_some_and(|id| {
                    !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())
                });
        } else if let Some(command) = line.strip_prefix("%begin ") {
            ensure!(command.len() <= 128, "Invalid SSH control command group");
            self.command = Some(command.to_owned());
        } else if let Some(command) = line.strip_prefix("%end ") {
            if self.pending.as_ref().is_some_and(|(id, _)| id == command) {
                self.completed = self.pending.take().map(|(_, status)| status);
            }
            self.command = None;
        } else if let Some(command) = line.strip_prefix("%error ") {
            ensure!(
                !self.pending.as_ref().is_some_and(|(id, _)| id == command),
                "SSH hook readiness query failed"
            );
            self.command = None;
        } else if let Some(metadata) = line.strip_prefix(&self.marker) {
            if let Some(command) = &self.command {
                self.pending = Some((command.clone(), readiness_status(metadata, allowed)?));
            }
        }
        if self.attached && self.completed.is_some() {
            self.reported = true;
            return Ok(self.completed.take());
        }
        Ok(None)
    }
}

fn readiness_status(metadata: &str, allowed: &[&str]) -> Result<TrackingStatus> {
    let fields: Vec<_> = metadata.split(' ').collect();
    ensure!(
        fields.first() == Some(&"3"),
        "Remote server is not an owned v3 SSH hook channel"
    );
    ensure!(fields.len() == 3, "Invalid remote SSH hook setup metadata");
    // This is a shared-server snapshot, not this controller's authorization:
    // another controller may have registered a different provider selection.
    let providers = |text: &str| -> Result<Vec<String>> {
        if text == "-" {
            return Ok(Vec::new());
        }
        let installed: Vec<_> = text.split(',').collect();
        ensure!(
            installed.len() <= 5
                && installed
                    .iter()
                    .all(|cli| crate::agent_registry::is_known_id(cli)),
            "Invalid provider in remote SSH hook registration snapshot"
        );
        let unique: std::collections::HashSet<_> = installed.iter().collect();
        ensure!(
            unique.len() == installed.len(),
            "Duplicate provider in remote SSH hook registration snapshot"
        );
        Ok(installed.into_iter().map(str::to_owned).collect())
    };
    let installed = providers(fields[1])?;
    let unavailable = providers(fields[2])?;
    let relevant: Vec<_> = unavailable
        .iter()
        .filter(|cli| allowed.contains(&cli.as_str()))
        .cloned()
        .collect();
    let installed_allowed = installed.iter().any(|cli| allowed.contains(&cli.as_str()));
    if installed_allowed && !relevant.is_empty() {
        return Ok(TrackingStatus::Partial {
            unavailable_providers: relevant,
        });
    }
    if !relevant.is_empty() {
        return Ok(TrackingStatus::Unavailable {
            reason: format!("Latest shared SSH setup reports unavailable registrations for {}; other hook traffic remains active", relevant.join(", ")),
        });
    }
    if !installed_allowed {
        return Ok(TrackingStatus::Unavailable {
            reason: "Latest shared SSH setup reported no allowed CLI registrations; control transport is connected but agent hook delivery is not verified".into(),
        });
    }
    Ok(TrackingStatus::Ready)
}

fn control_hook_frame(line: &str) -> Option<&str> {
    // Only an explicit control notification is a hook carrier. In particular,
    // a shell's %output text cannot impersonate a registered control event.
    line.strip_prefix("%message ")
        .filter(|message| message.starts_with("IT_AGENT_HOOK/3 "))
}

fn report_rejection(reported: &mut bool) {
    if !*reported {
        tracing::warn!(target: "ssh_hooks", "Dropping invalid, disallowed or over-budget v3 hook traffic; further diagnostics suppressed for this connection");
        *reported = true;
    }
}

async fn apply_routed_hook(
    state: &MasterStateInner,
    target: &SshTarget,
    route: RouteId,
    event: crate::ssh_hook_protocol::HookEvent,
) -> acp::Result<()> {
    if !state.ssh_hooks.enabled() {
        return Ok(());
    }
    // Revocation and application share this gate. A close that wins the race
    // cannot be followed by an old frame re-creating a row in its former pane.
    let mut routes = state.ssh_hooks.routes.lock().await;
    routes.expire(Instant::now());
    let Some(active) = routes
        .active
        .get(&route)
        .filter(|active| active.registration.target == *target && !active.registration.no_hooks)
    else {
        return Ok(());
    };
    super::ssh_sessions::hook_event(state, target, route, &active.registration.pane_id, event).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh_session_registry::{Snapshot, Source};

    fn registration(pane: uuid::Uuid) -> Registration {
        Registration {
            route: RouteId::new(),
            target: SshTarget::new("alice@remote", Some(2222)).unwrap(),
            pane_id: pane.to_string(),
            wrapper_pid: 42,
            no_hooks: false,
        }
    }

    struct Native {
        pid: u32,
    }

    #[async_trait::async_trait]
    impl crate::shell::wt_channel::WtChannel for Native {
        async fn request(&self, method: &str, _: serde_json::Value) -> Result<serde_json::Value> {
            match method {
                "get_process_status" => {
                    Ok(serde_json::json!({ "state": "running", "pid": self.pid }))
                }
                _ => Err(anyhow!("Unexpected native test request")),
            }
        }
        fn is_available(&self) -> bool {
            true
        }
    }

    fn state(enabled: bool, pid: u32) -> (Arc<MasterStateInner>, Arc<Native>) {
        let native = Arc::new(Native { pid });
        let mut state = super::super::tests::make_state();
        Arc::get_mut(&mut state).unwrap().wt = Some(native.clone());
        Arc::get_mut(&mut state).unwrap().ssh_hooks = Service::new(enabled);
        (state, native)
    }

    #[test]
    fn ssh_hook_routes_reject_takeover_revoke_late_registration_and_expire() {
        let now = Instant::now();
        let mut table = Routes::default();
        let first = registration(uuid::Uuid::new_v4());
        table.register(first.clone(), now).unwrap();
        table
            .register(first.clone(), now + Duration::from_secs(1))
            .unwrap();
        let mut takeover = first.clone();
        takeover.target = SshTarget::new("bob@remote", Some(2222)).unwrap();
        assert!(table.register(takeover, now).is_err());
        let mut next = first.clone();
        next.route = RouteId::new();
        assert!(table.register(next.clone(), now).is_err());
        assert!(table.revoke(&first).unwrap());
        assert!(table.register(first.clone(), now).is_err());
        table.register(next.clone(), now).unwrap();
        table.expire(now + ROUTE_LEASE);
        assert!(table.active.is_empty());
        table.register(next.clone(), now + ROUTE_LEASE).unwrap();
        let mut restarted = Routes::default();
        restarted.register(next, now).unwrap();
        assert_eq!(restarted.active.len(), 1);
        assert!(!format!("{first:?}").contains(&first.route.to_string()));
    }

    #[tokio::test]
    async fn ssh_hook_registration_validates_native_owner_and_retains_disabled_context() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (state, _) = state(false, 42);
                let registration = registration(uuid::Uuid::new_v4());
                let response = handle(
                    &state,
                    Request::Register {
                        registration: registration.clone(),
                    },
                )
                .await
                .unwrap();
                let response: Response = serde_json::from_str(response.0.get()).unwrap();
                assert_eq!(response.status, TrackingStatus::Disabled);
                assert!(state
                    .ssh_hooks
                    .active_route(&registration.target, registration.route)
                    .await
                    .is_some());
                assert!(state.ssh_hooks.channels.lock().await.is_empty());
                let mut wrong = registration.clone();
                wrong.route = RouteId::new();
                wrong.wrapper_pid += 1;
                assert!(handle(
                    &state,
                    Request::Register {
                        registration: wrong
                    }
                )
                .await
                .is_err());
                state.ssh_hooks.enabled.store(true, Ordering::Release);
                // An explicit opt-out stays disabled even after global consent changes.
                let mut opted = registration.clone();
                opted.pane_id = uuid::Uuid::new_v4().to_string();
                opted.route = RouteId::new();
                opted.no_hooks = true;
                let response = handle(
                    &state,
                    Request::Register {
                        registration: opted.clone(),
                    },
                )
                .await
                .unwrap();
                assert_eq!(
                    serde_json::from_str::<Response>(response.0.get())
                        .unwrap()
                        .status,
                    TrackingStatus::OptedOut
                );
                assert!(state
                    .ssh_hooks
                    .active_route(&opted.target, opted.route)
                    .await
                    .is_none());
                handle(
                    &state,
                    Request::Revoke {
                        registration: registration.clone(),
                    },
                )
                .await
                .unwrap();
                assert!(state
                    .ssh_hooks
                    .active_route(&registration.target, registration.route)
                    .await
                    .is_none());
            })
            .await;
    }

    #[tokio::test]
    async fn ssh_hook_ensure_deduplicates_inflight_target_without_starting_another_reader() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (state, _) = state(true, 42);
                let registration = registration(uuid::Uuid::new_v4());
                state
                    .ssh_hooks
                    .routes
                    .lock()
                    .await
                    .register(registration.clone(), Instant::now())
                    .unwrap();
                let cancellation = CancellationToken::new();
                let stopped = cancellation.clone();
                let task = tokio::task::spawn_local(async move { stopped.cancelled().await });
                let (_status, receiver) = watch::channel(TrackingStatus::Connecting);
                state.ssh_hooks.channels.lock().await.insert(
                    registration.target.clone(),
                    Channel {
                        cancellation: cancellation.clone(),
                        reconcile: Arc::new(Notify::new()),
                        reconciling: Arc::new(AtomicBool::new(true)),
                        status: receiver,
                        task: AbortOnDropHandle::new(task),
                    },
                );
                let (a, b) = tokio::join!(
                    ensure_target(&state, &registration.target),
                    ensure_target(&state, &registration.target),
                );
                assert_eq!(a, TrackingStatus::Connecting);
                assert_eq!(b, TrackingStatus::Connecting);
                assert_eq!(state.ssh_hooks.channels.lock().await.len(), 1);
                handle(&state, Request::Revoke { registration })
                    .await
                    .unwrap();
                assert!(cancellation.is_cancelled());
            })
            .await;
    }

    #[tokio::test]
    async fn ssh_hook_target_and_route_fence_prevent_late_or_foreign_events() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (state, _) = state(true, 42);
                let first = registration(uuid::Uuid::new_v4());
                state
                    .ssh_hooks
                    .routes
                    .lock()
                    .await
                    .register(first.clone(), Instant::now())
                    .unwrap();
                let event = crate::ssh_hook_protocol::HookEvent {
                    cli_source: "copilot".into(),
                    event: "agent.session.start".into(),
                    raw_session_id: "sid".into(),
                    payload: serde_json::json!({"cwd": "/home/alice"}),
                };
                apply_routed_hook(&state, &first.target, first.route, event.clone())
                    .await
                    .unwrap();
                handle(
                    &state,
                    Request::Revoke {
                        registration: first.clone(),
                    },
                )
                .await
                .unwrap();
                let mut second = first.clone();
                second.route = RouteId::new();
                state
                    .ssh_hooks
                    .routes
                    .lock()
                    .await
                    .register(second.clone(), Instant::now())
                    .unwrap();
                let mut late = event.clone();
                late.raw_session_id = "late".into();
                apply_routed_hook(&state, &first.target, first.route, late.clone())
                    .await
                    .unwrap();
                apply_routed_hook(
                    &state,
                    &SshTarget::new("bob@remote", Some(2222)).unwrap(),
                    second.route,
                    late,
                )
                .await
                .unwrap();
                apply_routed_hook(&state, &second.target, second.route, event)
                    .await
                    .unwrap();
                let source = Source {
                    target: first.target,
                    agent_id: "copilot".into(),
                };
                let response = super::super::ssh_sessions::handle(
                    &state,
                    crate::ssh_session_registry::Request::List {
                        source,
                        refresh_history: false,
                    },
                )
                .await
                .unwrap();
                let snapshot: Snapshot = serde_json::from_str(response.0.get()).unwrap();
                assert_eq!(snapshot.sessions.len(), 1);
                assert_eq!(snapshot.sessions[0].session_id.0.as_ref(), "sid");
                assert_eq!(
                    snapshot.sessions[0].pane_session_id.as_deref(),
                    Some(first.pane_id.as_str())
                );
                assert!(state.registry.snapshot().await.is_empty());
            })
            .await;
    }

    #[tokio::test]
    async fn ssh_hook_control_lines_and_upload_are_bounded_and_private() {
        let mut reader = BufReader::new(std::io::Cursor::new(vec![b'x'; MAX_CONTROL_LINE + 1]));
        assert!(read_line_bounded(&mut reader, &mut Vec::new())
            .await
            .is_err());
        let route = RouteId::new();
        let payload = format!("IT_AGENT_HOOK/3 {route} copilot agent.stop tx 0 1 e30=");
        assert!(control_hook_frame(&format!("%output %1 {payload}")).is_none());
        assert!(control_hook_frame(&payload).is_none());
        assert_eq!(
            control_hook_frame(&format!("%message {payload}")),
            Some(payload.as_str())
        );
        let script = bootstrap_command(123, 456, &["copilot", "claude"]).unwrap();
        assert!(script.ends_with("it-ssh-upload 123 456 'copilot,claude'"));
        assert!(script.contains("count=\"$1\" of=\"$it_stage/install-remote-hooks.sh\""));
        assert!(script.contains("count=\"$2\" of=\"$it_stage/it-agent-hook.sh\""));
        assert!(script.contains("wc -c"));
        assert!(script.contains("--allowed-clis \"$3\""));
        assert!(script.contains("sh -lc "));
        assert!(script.contains("exec sh \"$@\""));
        assert!(script.contains("it-ssh-setup"));
        assert!(!script.contains("mkdir"));
        assert!(!script.contains(&route.to_string()));
        assert!(!script.contains("kill-server"));
        assert!(!script.contains("send-keys"));
        assert!(bootstrap_command(0, 456, &["copilot"]).is_err());
        assert!(bootstrap_command(123, 262145, &["copilot"]).is_err());
        assert!(bootstrap_command(123, 456, &[]).is_err());
        assert!(bootstrap_command(123, 456, &["copilot", "copilot"]).is_err());
        assert!(bootstrap_command(123, 456, &["custom:blocked"]).is_err());
    }

    #[test]
    fn ssh_hook_readiness_requires_owned_protocol_nonce_completed_command_and_attach() {
        let mut readiness = Readiness::new("nonce");
        let allowed = ["copilot", "claude"];
        assert!(readiness
            .observe("IT_HOOK_READY/3 nonce 3 copilot -", &allowed)
            .unwrap()
            .is_none());
        assert!(readiness
            .observe("%begin 123 1 1", &allowed)
            .unwrap()
            .is_none());
        assert!(readiness
            .observe("IT_HOOK_READY/3 wrong 3 copilot -", &allowed)
            .unwrap()
            .is_none());
        assert!(readiness
            .observe("IT_HOOK_READY/3 nonce 3 copilot -", &allowed)
            .unwrap()
            .is_none());
        assert!(readiness
            .observe("%end 123 1 1", &allowed)
            .unwrap()
            .is_none());
        assert_eq!(
            readiness
                .observe("%session-changed $0 it-hooks", &allowed)
                .unwrap(),
            Some(TrackingStatus::Ready)
        );
        assert!(readiness_status("2 copilot -", &allowed).is_err());
        assert!(readiness_status("3 unexpected", &allowed).is_err());
        assert!(readiness_status("3 copilot unknown", &allowed).is_err());
        assert!(readiness_status("3 copilot,copilot -", &allowed).is_err());
        assert!(matches!(
            readiness_status("3 claude -", &["copilot"]).unwrap(),
            TrackingStatus::Unavailable { .. }
        ));
        assert_eq!(
            readiness_status("3 copilot claude", &allowed).unwrap(),
            TrackingStatus::Partial {
                unavailable_providers: vec!["claude".into()]
            }
        );
        assert_eq!(
            readiness_status("3 copilot claude", &["copilot"]).unwrap(),
            TrackingStatus::Ready
        );
        assert!(matches!(
            readiness_status("3 - -", &allowed).unwrap(),
            TrackingStatus::Unavailable { .. }
        ));
        assert!(readiness_status("3  ", &allowed).is_err());
    }

    #[tokio::test]
    async fn ssh_hook_control_reader_retains_partial_lines_when_cancelled() {
        let (mut writer, input) = tokio::io::duplex(128);
        let mut reader = BufReader::new(input);
        let mut line = Vec::new();
        writer.write_all(b"%message IT_AGENT_").await.unwrap();
        assert!(tokio::time::timeout(
            Duration::from_millis(10),
            read_line_bounded(&mut reader, &mut line),
        )
        .await
        .is_err());
        assert_eq!(line, b"%message IT_AGENT_");
        writer
            .write_all(b"HOOK/3 complete\n%end next\r\n")
            .await
            .unwrap();
        assert!(read_line_bounded(&mut reader, &mut line).await.unwrap());
        assert_eq!(
            std::mem::take(&mut line),
            b"%message IT_AGENT_HOOK/3 complete"
        );
        assert!(read_line_bounded(&mut reader, &mut line).await.unwrap());
        assert_eq!(line, b"%end next");
    }

    #[test]
    fn ssh_hook_notifications_are_demultiplexed_while_the_nonce_reply_is_pending() {
        let mut readiness = Readiness::new("nonce");
        let frame =
            "IT_AGENT_HOOK/3 00000000-1111-2222-3333-444444444444 copilot agent.stop tx 0 1 ";
        let line = format!("%message {frame}");
        assert!(readiness.hook_frame(&line).is_none());
        assert!(readiness
            .observe("%session-changed $0 it-hooks", &["copilot"])
            .unwrap()
            .is_none());
        assert!(readiness
            .observe("%begin 123 1 1", &["copilot"])
            .unwrap()
            .is_none());
        assert!(readiness.observe(&line, &["copilot"]).unwrap().is_none());
        assert_eq!(readiness.hook_frame(&line), Some(frame));
        assert!(readiness.hook_frame("%output %0 not-a-hook").is_none());
    }

    #[tokio::test]
    async fn ssh_hook_configuration_coalesces_windows_and_disables_without_ending_foreground() {
        use futures::FutureExt;
        tokio::task::LocalSet::new().run_until(async {
            let (state, _) = state(false, 42);
            let registration = registration(uuid::Uuid::new_v4());
            handle(&state, Request::Register { registration: registration.clone() }).await.unwrap();
            let cancellation = CancellationToken::new();
            let stopped = cancellation.clone();
            let task = tokio::task::spawn_local(async move { stopped.cancelled().await });
            let reconcile = Arc::new(Notify::new());
            let reconciling = Arc::new(AtomicBool::new(false));
            let (_status, receiver) = watch::channel(TrackingStatus::Ready);
            state.ssh_hooks.channels.lock().await.insert(registration.target.clone(), Channel {
                cancellation: cancellation.clone(), reconcile: Arc::clone(&reconcile),
                reconciling: Arc::clone(&reconciling),
                status: receiver, task: AbortOnDropHandle::new(task),
            });
            let enabled = serde_json::json!({ "method": "ssh_hooks_configuration", "params": { "enabled": true } });
            super::super::handle_master_wt_event(&state, enabled.clone()).await;
            super::super::handle_master_wt_event(&state, enabled).await;
            assert!(state.ssh_hooks.enabled());
            assert!(reconciling.load(Ordering::Acquire));
            assert!(reconcile.notified().now_or_never().is_some());
            assert!(reconcile.notified().now_or_never().is_none());
            assert_eq!(state.ssh_hooks.channels.lock().await.len(), 1);
            apply_routed_hook(&state, &registration.target, registration.route, crate::ssh_hook_protocol::HookEvent {
                cli_source: "copilot".into(), event: "agent.prompt.submit".into(),
                raw_session_id: "foreground".into(), payload: serde_json::json!({}),
            }).await.unwrap();
            super::super::handle_master_wt_event(&state, serde_json::json!({
                "method": "ssh_hooks_configuration", "params": { "enabled": false }
            })).await;
            assert!(!state.ssh_hooks.enabled());
            assert!(cancellation.is_cancelled());
            assert!(state.ssh_hooks.active_route(&registration.target, registration.route).await.is_some());
            let response = handle(&state, Request::Ensure).await.unwrap();
            assert_eq!(serde_json::from_str::<Response>(response.0.get()).unwrap().status, TrackingStatus::Disabled);
            let response = super::super::ssh_sessions::handle(&state, crate::ssh_session_registry::Request::List {
                source: Source { target: registration.target, agent_id: "copilot".into() }, refresh_history: false,
            }).await.unwrap();
            let snapshot: Snapshot = serde_json::from_str(response.0.get()).unwrap();
            assert_eq!(snapshot.sessions[0].status, Some(crate::agent_sessions::AgentStatus::Working));
            assert_eq!(snapshot.sessions[0].pane_session_id.as_deref(), Some(registration.pane_id.as_str()));
        }).await;
    }
}
