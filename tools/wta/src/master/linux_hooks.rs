//! One bounded installation worker per active Linux target. Ordinary WSL has
//! installation only: this service never creates an agent-event reader.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{anyhow, ensure, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{watch, Mutex};
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use super::MasterStateInner;
use crate::linux_hooks::{
    Binding, InstallState, ProviderStatus, Request, Response, Target, TargetStatus, PROVIDERS,
};

const INSTALL_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_TARGETS: usize = 32;

#[derive(Default)]
pub(super) struct Service {
    bindings: Mutex<HashMap<String, Binding>>,
    jobs: Mutex<HashMap<Target, Arc<Job>>>,
    closed: Mutex<VecDeque<String>>,
    #[cfg(test)]
    allow_install_subprocesses: bool,
}

#[derive(Default)]
struct Work {
    pending: BTreeSet<String>,
    running: BTreeSet<String>,
    states: BTreeMap<String, InstallState>,
    received: BTreeMap<String, InstallState>,
    active: bool,
}

struct Job {
    work: Mutex<Work>,
    changed: watch::Sender<Vec<ProviderStatus>>,
    cancellation: CancellationToken,
}

fn publish(job: &Job, work: &Work) {
    job.changed.send_replace(
        work.states
            .iter()
            .map(|(cli, status)| ProviderStatus {
                cli: cli.clone(),
                status: status.clone(),
            })
            .collect(),
    );
}

pub(super) fn selected_clis(state: &MasterStateInner, cli: Option<&str>) -> Result<Vec<String>> {
    ensure!(
        cli.is_none_or(|cli| PROVIDERS.contains(&cli)),
        "Unsupported hook provider"
    );
    let selected: Vec<_> = PROVIDERS
        .iter()
        .filter(|candidate| cli.is_none_or(|cli| cli == **candidate))
        .filter(|candidate| {
            state
                .allowed_agent_ids
                .as_ref()
                .is_none_or(|allowed| allowed.contains(**candidate))
        })
        .map(|cli| (*cli).to_owned())
        .collect();
    ensure!(
        cli.is_none() || !selected.is_empty(),
        "Hook provider is blocked by policy"
    );
    Ok(selected)
}

pub(super) async fn register(state: &Arc<MasterStateInner>, mut binding: Binding) -> Result<()> {
    binding.validate()?;
    ensure!(
        !state
            .linux_hooks
            .closed
            .lock()
            .await
            .contains(&binding.pane_id),
        "Linux hook pane is closed"
    );
    let target = binding.target.clone();
    let install = binding.native_tmux || matches!(target, Target::Wsl { .. });
    let changed = {
        let mut bindings = state.linux_hooks.bindings.lock().await;
        ensure!(
            bindings.contains_key(&binding.pane_id) || bindings.len() < 256,
            "Linux hook pane limit reached"
        );
        let changed = !bindings.get(&binding.pane_id).is_some_and(|old| {
            old.target == binding.target && old.native_tmux == binding.native_tmux
        });
        bindings.insert(binding.pane_id.clone(), binding);
        changed
    };
    if install && state.ssh_hooks.enabled() {
        ensure_install(state, &target, selected_clis(state, None)?, changed).await?;
    }
    Ok(())
}

async fn active_targets(state: &MasterStateInner) -> HashSet<Target> {
    let bindings: Vec<_> = state
        .linux_hooks
        .bindings
        .lock()
        .await
        .values()
        .cloned()
        .collect();
    let mut targets = HashSet::new();
    for binding in bindings {
        if binding.native_tmux || matches!(binding.target, Target::Wsl { .. }) {
            targets.insert(binding.target);
        }
    }
    for target in state.ssh_hooks.active_targets().await {
        let target = Target::Ssh { target };
        targets.insert(target);
    }
    targets
}

pub(super) async fn configure(state: &Arc<MasterStateInner>, enabled: bool) {
    if !enabled {
        for job in state
            .linux_hooks
            .jobs
            .lock()
            .await
            .drain()
            .map(|(_, job)| job)
        {
            job.cancellation.cancel();
        }
        return;
    }
    request_discovery();
    for target in active_targets(state).await {
        match selected_clis(state, None) {
            Ok(clis) => {
                if let Err(error) = ensure_install(state, &target, clis, true).await {
                    tracing::warn!(target: "linux_hooks", %error, "Could not reconcile Linux hooks");
                }
            }
            Err(error) => {
                tracing::warn!(target: "linux_hooks", %error, "Linux hook policy unavailable")
            }
        }
    }
}

pub(super) fn request_discovery() {
    crate::wt_protocol_events::send(
        serde_json::json!({
            "type": "event", "method": "linux_hooks_discover", "params": {}
        })
        .to_string(),
    );
}

pub(super) async fn listener_ready(state: &MasterStateInner) {
    state.linux_hooks.bindings.lock().await.clear();
    state.linux_hooks.closed.lock().await.clear();
    let ssh_targets = state.ssh_hooks.active_targets().await;
    state.linux_hooks.jobs.lock().await.retain(|target, job| {
        let keep = matches!(target, Target::Ssh { target } if ssh_targets.contains(target));
        if !keep {
            job.cancellation.cancel();
        }
        keep
    });
    request_discovery();
}

pub(super) async fn pane_closed(state: &MasterStateInner, pane: &str) {
    let pane = crate::agent_sessions::pane_key(pane);
    {
        let mut closed = state.linux_hooks.closed.lock().await;
        if closed.len() == 256 {
            closed.pop_front();
        }
        closed.push_back(pane.clone());
    }
    state.linux_hooks.bindings.lock().await.remove(&pane);
    let active = active_targets(state).await;
    state.linux_hooks.jobs.lock().await.retain(|target, job| {
        let keep = active.contains(target);
        if !keep {
            job.cancellation.cancel();
        }
        keep
    });
}

pub(super) async fn pane_connected(state: &MasterStateInner, pane: &str) {
    state
        .linux_hooks
        .closed
        .lock()
        .await
        .retain(|closed| closed != &crate::agent_sessions::pane_key(pane));
}

pub(super) async fn rename_tab(state: &MasterStateInner, old: &str, new: &str) {
    for binding in state.linux_hooks.bindings.lock().await.values_mut() {
        if binding.tab_id == old {
            binding.tab_id = new.to_owned();
        }
    }
}

pub(super) async fn ensure_install(
    state: &Arc<MasterStateInner>,
    target: &Target,
    clis: Vec<String>,
    refresh: bool,
) -> Result<watch::Receiver<Vec<ProviderStatus>>> {
    target.validate()?;
    ensure!(state.ssh_hooks.enabled(), "Session Management is disabled");
    ensure!(
        clis.iter().all(|cli| PROVIDERS.contains(&cli.as_str())),
        "Invalid hook provider"
    );
    let job = {
        let mut jobs = state.linux_hooks.jobs.lock().await;
        ensure!(
            jobs.contains_key(target) || jobs.len() < MAX_TARGETS,
            "Linux hook target limit reached"
        );
        Arc::clone(jobs.entry(target.clone()).or_insert_with(|| {
            Arc::new(Job {
                work: Mutex::new(Work::default()),
                changed: watch::channel(Vec::new()).0,
                cancellation: CancellationToken::new(),
            })
        }))
    };
    let receiver = job.changed.subscribe();
    let mut work = job.work.lock().await;
    for cli in clis {
        if !work.running.contains(&cli) && (refresh || !work.states.contains_key(&cli)) {
            work.states.insert(cli.clone(), InstallState::Checking);
            work.pending.insert(cli);
        }
    }
    publish(&job, &work);
    if !work.active && !work.pending.is_empty() {
        work.active = true;
        tokio::task::spawn_local(worker(
            Arc::downgrade(state),
            target.clone(),
            Arc::clone(&job),
        ));
    }
    Ok(receiver)
}

async fn worker(state: Weak<MasterStateInner>, target: Target, job: Arc<Job>) {
    loop {
        let clis = {
            let mut work = job.work.lock().await;
            if work.pending.is_empty() || job.cancellation.is_cancelled() {
                work.active = false;
                return;
            }
            work.running = std::mem::take(&mut work.pending);
            work.received.clear();
            work.running.iter().cloned().collect::<Vec<_>>()
        };
        let Some(current) = state.upgrade() else {
            return;
        };
        if !current.ssh_hooks.enabled() {
            return;
        }
        #[cfg(test)]
        let allow_subprocesses = current.linux_hooks.allow_install_subprocesses;
        #[cfg(not(test))]
        let allow_subprocesses = true;
        drop(current);
        let result = if allow_subprocesses {
            run_installer(&target, &clis, &job).await
        } else {
            Err(anyhow!("Hook subprocesses are disabled in unit tests; use the isolated Linux installer fixtures"))
        };
        let mut work = job.work.lock().await;
        let completed = result.is_ok();
        if let Err(error) = result {
            tracing::warn!(target: "linux_hooks", %error, "Linux hook installation did not complete");
        }
        complete_work(&mut work, &clis, completed, job.cancellation.is_cancelled());
        work.running.clear();
        publish(&job, &work);
    }
}

fn complete_work(work: &mut Work, clis: &[String], completed: bool, cancelled: bool) {
    let partial = work
        .received
        .values()
        .any(|status| matches!(status, InstallState::Unavailable { .. }));
    for cli in clis {
        let status = match work.received.remove(cli) {
            Some(InstallState::Installed) if !cancelled && (completed || partial) => {
                InstallState::Installed
            }
            Some(
                status @ (InstallState::Disabled
                | InstallState::NotFound
                | InstallState::Unavailable { .. }),
            ) if !cancelled => status,
            _ => InstallState::Unavailable {
                reason: if cancelled {
                    "cancelled"
                } else {
                    "installation-incomplete"
                }
                .to_owned(),
            },
        };
        work.states.insert(cli.clone(), status);
    }
}

fn record_result(work: &mut Work, cli: &str, status: &InstallState) {
    if *status == InstallState::Installing {
        work.states.insert(cli.to_owned(), status.clone());
    } else {
        work.received.insert(cli.to_owned(), status.clone());
    }
}

pub(super) async fn wait_for(
    mut receiver: watch::Receiver<Vec<ProviderStatus>>,
    clis: &[String],
) -> Result<Vec<ProviderStatus>> {
    tokio::time::timeout(INSTALL_TIMEOUT + Duration::from_secs(5), async {
        loop {
            let values = receiver.borrow_and_update().clone();
            if clis.iter().all(|cli| {
                values.iter().any(|p| {
                    p.cli == *cli
                        && !matches!(p.status, InstallState::Checking | InstallState::Installing)
                })
            }) {
                return Ok(values
                    .into_iter()
                    .filter(|p| clis.contains(&p.cli))
                    .collect());
            }
            receiver
                .changed()
                .await
                .context("Linux hook worker closed")?;
        }
    })
    .await
    .context("Linux hook installation timed out")?
}

pub(super) async fn handle(
    state: &Arc<MasterStateInner>,
    request: Request,
) -> agent_client_protocol::Result<agent_client_protocol::schema::v1::ExtResponse> {
    let result = handle_inner(state, request)
        .await
        .map_err(|error| agent_client_protocol::Error::invalid_request().data(error.to_string()))?;
    Ok(agent_client_protocol::schema::v1::ExtResponse::new(
        serde_json::value::to_raw_value(&result)
            .map_err(|_| agent_client_protocol::Error::internal_error())?
            .into(),
    ))
}

async fn handle_inner(state: &Arc<MasterStateInner>, request: Request) -> Result<Response> {
    if !state.ssh_hooks.enabled() {
        return Ok(Response::default());
    }
    let mut result = Response {
        enabled: true,
        targets: Vec::new(),
    };
    match request {
        Request::Prepare { target, cli } => {
            ensure!(
                active_targets(state).await.contains(&target),
                "Linux target is not active"
            );
            let clis = selected_clis(state, Some(&cli))?;
            let receiver = ensure_install(state, &target, clis.clone(), false).await?;
            result.targets.push(TargetStatus {
                target,
                providers: wait_for(receiver, &clis).await?,
            });
        }
        Request::PrepareWsl { distro, cli } => {
            ensure!(
                active_targets(state)
                    .await
                    .iter()
                    .any(|target| matches!(target,
                Target::Wsl { distro: active, .. } if active == &distro)),
                "WSL distribution is not active"
            );
            let clis = selected_clis(state, Some(&cli))?;
            let user = default_wsl_user(&distro).await?;
            let target = Target::Wsl { distro, user };
            let receiver = ensure_install(state, &target, clis.clone(), false).await?;
            result.targets.push(TargetStatus {
                target,
                providers: wait_for(receiver, &clis).await?,
            });
        }
        Request::Snapshot {
            tab_id,
            cli,
            pane_id,
        } => {
            ensure!(
                !tab_id.is_empty() && tab_id.len() <= 128,
                "Invalid hook status tab"
            );
            let pane_id = pane_id
                .map(|pane| {
                    let id = uuid::Uuid::parse_str(&pane)?;
                    ensure!(!id.is_nil(), "Invalid hook status pane");
                    Ok::<_, anyhow::Error>(id.to_string())
                })
                .transpose()?;
            let clis = selected_clis(state, Some(&cli))?;
            let bindings: Vec<_> = state
                .linux_hooks
                .bindings
                .lock()
                .await
                .values()
                .filter(|binding| {
                    binding.tab_id == tab_id
                        && pane_id.as_ref().is_none_or(|pane| &binding.pane_id == pane)
                })
                .cloned()
                .collect();
            let mut targets = HashSet::new();
            let mut managed = HashSet::new();
            for binding in bindings {
                if binding.native_tmux
                    || matches!(binding.target, Target::Wsl { .. })
                    || state.ssh_hooks.tracks_pane(&binding.pane_id).await
                {
                    if !binding.native_tmux && matches!(binding.target, Target::Ssh { .. }) {
                        managed.insert(binding.target.clone());
                    }
                    targets.insert(binding.target);
                }
            }
            if pane_id.is_none() && targets.len() > 1 {
                return Ok(result);
            }
            let jobs = state.linux_hooks.jobs.lock().await;
            for target in targets {
                if let Some(job) = jobs.get(&target) {
                    result.targets.push(TargetStatus {
                        target,
                        providers: job
                            .changed
                            .borrow()
                            .iter()
                            .filter(|p| clis.contains(&p.cli))
                            .cloned()
                            .collect(),
                    });
                }
            }
            drop(jobs);
            for target in &mut result.targets {
                if !managed.contains(&target.target) {
                    continue;
                }
                let Target::Ssh { target: ssh } = &target.target else {
                    continue;
                };
                let tracking = state.ssh_hooks.tracking_status(ssh).await;
                for provider in &mut target.providers {
                    if provider.status != InstallState::Installed {
                        continue;
                    }
                    let reason = match &tracking {
                        Some(crate::ssh_hook_protocol::TrackingStatus::Unavailable { .. }) => {
                            Some("transport-unavailable")
                        }
                        Some(crate::ssh_hook_protocol::TrackingStatus::Partial {
                            unavailable_providers,
                        }) if unavailable_providers.contains(&provider.cli) => {
                            Some("transport-runtime-unavailable")
                        }
                        _ => None,
                    };
                    if let Some(reason) = reason {
                        provider.status = InstallState::Unavailable {
                            reason: reason.to_owned(),
                        };
                    }
                }
            }
        }
        Request::Install { cli } => {
            let clis = selected_clis(state, cli.as_deref())?;
            let mut pending = Vec::new();
            for target in active_targets(state).await {
                match ensure_install(state, &target, clis.clone(), true).await {
                    Ok(receiver) => pending.push((target, receiver)),
                    Err(error) => {
                        tracing::warn!(target: "linux_hooks", %error, "Could not start manual hook installation");
                        result.targets.push(TargetStatus {
                            target,
                            providers: clis
                                .iter()
                                .map(|cli| ProviderStatus {
                                    cli: cli.clone(),
                                    status: InstallState::Unavailable {
                                        reason: "installation-not-started".to_owned(),
                                    },
                                })
                                .collect(),
                        });
                    }
                }
            }
            let completed = futures::future::join_all(pending.into_iter().map(|(target, receiver)| {
                let clis = clis.clone();
                async move {
                    let providers = match wait_for(receiver, &clis).await {
                        Ok(providers) => providers,
                        Err(error) => {
                            tracing::warn!(target: "linux_hooks", %error, "Manual hook installation timed out");
                            clis.iter()
                                .map(|cli| ProviderStatus {
                                    cli: cli.clone(),
                                    status: InstallState::Unavailable {
                                        reason: "installation-timeout".to_owned(),
                                    },
                                })
                                .collect()
                        }
                    };
                    TargetStatus { target, providers }
                }
            })).await;
            result.targets.extend(completed);
            for target in &result.targets {
                if let Target::Ssh { target: ssh } = &target.target {
                    let installed: Vec<_> = target
                        .providers
                        .iter()
                        .filter(|provider| provider.status == InstallState::Installed)
                        .map(|provider| provider.cli.clone())
                        .collect();
                    super::ssh_hooks::retry_transport(state, ssh, Some(&installed)).await;
                }
            }
        }
    }
    result.targets.sort_by_key(|status| status.target.label());
    Ok(result)
}

fn installer_command(
    installer_bytes: usize,
    hook_bytes: usize,
    nonce: &str,
    clis: &[String],
) -> Result<String> {
    ensure!(
        (1..=262144).contains(&installer_bytes) && (1..=262144).contains(&hook_bytes),
        "Invalid hook bundle size"
    );
    ensure!(
        !clis.is_empty() && clis.iter().all(|cli| PROVIDERS.contains(&cli.as_str())),
        "Invalid hook scope"
    );
    let script = r#"set -eu
umask 077
test "$(uname -s)" = Linux || exit 1
it_stage=$(mktemp -d -- "${TMPDIR:-/tmp}/it-hooks-upload.XXXXXXXXXXXX")
trap 'rm -f -- "$it_stage/install-remote-hooks.sh" "$it_stage/it-agent-hook.sh"; rmdir -- "$it_stage"' 0
trap 'exit 1' HUP INT TERM
timeout --kill-after=1 20 dd bs=1 count="$1" of="$it_stage/install-remote-hooks.sh" 2>/dev/null
[ "$(wc -c < "$it_stage/install-remote-hooks.sh")" -eq "$1" ]
timeout --kill-after=1 20 dd bs=1 count="$2" of="$it_stage/it-agent-hook.sh" 2>/dev/null
[ "$(wc -c < "$it_stage/it-agent-hook.sh")" -eq "$2" ]
it_home=$(getent passwd "$(id -u)" | cut -d: -f6)
sh -lc 'exec sh "$@"' it-hooks-setup "$it_stage/install-remote-hooks.sh" --install-only --hook-source "$it_stage/it-agent-hook.sh" --login-home "$it_home" --allowed-clis "$3" --report-nonce "$4"
"#;
    Ok(format!(
        "sh -c {} it-hooks-upload {installer_bytes} {hook_bytes} {} {}",
        crate::coordinator::sh_quote(script),
        crate::coordinator::sh_quote(&clis.join(",")),
        crate::coordinator::sh_quote(nonce)
    ))
}

async fn run_installer(target: &Target, clis: &[String], job: &Job) -> Result<()> {
    let (installer, hook) =
        tokio::task::spawn_blocking(crate::agent_hooks_installer::remote_hook_assets).await??;
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let script = installer_command(installer.len(), hook.len(), &nonce, clis)?;
    let mut command = match target {
        Target::Ssh { target } => {
            let mut command =
                tokio::process::Command::new(crate::ssh_sessions::system_ssh_executable()?);
            command.args(crate::ssh_sessions::ssh_arguments(target, false, &script));
            crate::ssh_sessions::configure_ssh_environment(&mut command, std::env::vars_os());
            command
        }
        Target::Wsl { distro, user } => {
            let mut command = tokio::process::Command::new(crate::linux_hooks::wsl_executable()?);
            command.args([
                "--distribution",
                distro,
                "--user",
                user,
                "--exec",
                "sh",
                "-c",
                &script,
            ]);
            command.env("WSL_UTF8", "1");
            command
        }
    };
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let mut child = command.spawn().context("Start Linux hook installer")?;
    let mut input = child.stdin.take().context("Hook upload stdin missing")?;
    let output = child
        .stdout
        .take()
        .context("Hook installation stdout missing")?;
    let mut errors = child
        .stderr
        .take()
        .context("Hook installation stderr missing")?;
    let _errors = AbortOnDropHandle::new(tokio::task::spawn_local(async move {
        if let Err(error) = tokio::io::copy(&mut errors, &mut tokio::io::sink()).await {
            tracing::debug!(target: "linux_hooks", %error, "Installer diagnostic stream closed");
        }
    }));
    let result = tokio::select! {
        _ = job.cancellation.cancelled() => Err(anyhow!("Linux hook installation cancelled")),
        result = tokio::time::timeout(INSTALL_TIMEOUT, async {
            input.write_all(&installer).await?;
            input.write_all(&hook).await?;
            input.shutdown().await?;
            let mut lines = BufReader::new(output.take(64 * 1024 + 1)).lines();
            let mut bytes = 0;
            while let Some(line) = lines.next_line().await? {
                bytes += line.len() + 1;
                ensure!(bytes <= 64 * 1024 && line.len() <= 4096, "Linux hook report exceeded its limit");
                if let Some((cli, status)) = crate::linux_hooks::parse_report(&line, &nonce, clis)? {
                    let mut work = job.work.lock().await;
                    for provider in clis.iter().filter(|provider| cli == "all" || *provider == cli) {
                        record_result(&mut work, provider, &status);
                    }
                    publish(job, &work);
                }
            }
            ensure!(child.wait().await?.success(), "Linux hook installer failed");
            Ok(())
        }) => result.context("Linux hook installer timed out")?,
    };
    if child.try_wait()?.is_none() {
        tokio::time::timeout(Duration::from_secs(3), child.kill())
            .await
            .context("Reap Linux hook installer")??;
    }
    result
}

async fn default_wsl_user(distro: &str) -> Result<String> {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let marker = format!("IT_HOOK_USER/1 {nonce} ");
    let script = format!(
        "printf '%s' {}; id -un",
        crate::coordinator::sh_quote(&marker)
    );
    let mut command = tokio::process::Command::new(crate::linux_hooks::wsl_executable()?);
    command
        .args(["--distribution", distro, "--exec", "sh", "-c", &script])
        .env("WSL_UTF8", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let mut child = command.spawn().context("Resolve WSL resume user")?;
    let mut output = child
        .stdout
        .take()
        .context("WSL user probe stdout missing")?
        .take(4097);
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        let mut bytes = Vec::new();
        output.read_to_end(&mut bytes).await?;
        ensure!(
            bytes.len() <= 4096 && child.wait().await?.success(),
            "WSL user probe failed"
        );
        let text = std::str::from_utf8(&bytes)?;
        let user = text
            .lines()
            .find_map(|line| line.strip_prefix(&marker))
            .context("WSL user probe response missing")?;
        Target::Wsl {
            distro: distro.to_owned(),
            user: user.to_owned(),
        }
        .validate()?;
        Ok(user.to_owned())
    })
    .await;
    if child.try_wait()?.is_none() {
        child.kill().await.context("Reap WSL user probe")?;
    }
    result.context("WSL user probe timed out")?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(enabled: bool) -> Arc<MasterStateInner> {
        let mut state = super::super::tests::make_state();
        Arc::get_mut(&mut state).unwrap().ssh_hooks =
            super::super::ssh_hooks::Service::new(enabled);
        state
    }

    fn job(states: &[(&str, InstallState)], active: bool) -> Arc<Job> {
        let work = Work {
            states: states
                .iter()
                .map(|(cli, status)| ((*cli).to_owned(), status.clone()))
                .collect(),
            active,
            ..Work::default()
        };
        let job = Arc::new(Job {
            work: Mutex::new(work),
            changed: watch::channel(Vec::new()).0,
            cancellation: CancellationToken::new(),
        });
        job
    }

    fn binding(tab: &str, user: &str) -> Binding {
        Binding {
            pane_id: uuid::Uuid::new_v4().to_string(),
            tab_id: tab.to_owned(),
            window_id: "1".to_owned(),
            target: Target::Wsl {
                distro: "Ubuntu".to_owned(),
                user: user.to_owned(),
            },
            native_tmux: false,
        }
    }

    #[tokio::test]
    async fn disabled_management_registers_identity_without_installing() {
        let state = state(false);
        register(&state, binding("tab", "alice")).await.unwrap();
        assert!(state.linux_hooks.jobs.lock().await.is_empty());
        assert!(
            !handle_inner(
                &state,
                Request::Snapshot {
                    tab_id: "tab".into(),
                    cli: "copilot".into(),
                    pane_id: None,
                }
            )
            .await
            .unwrap()
            .enabled
        );
    }

    #[tokio::test]
    async fn listener_recovery_rediscovers_targets_without_starting_agents() {
        let _capture = crate::wt_protocol_events::capture_test_published_events();
        let ready = serde_json::json!({ "method": "wt_listener_ready", "params": {} });
        super::super::handle_master_wt_event(&state(false), ready.clone()).await;
        assert!(crate::wt_protocol_events::take_test_published_events().is_empty());
        let state = state(true);
        let stale = binding("closed-during-outage", "alice");
        let stale_job = job(&[("copilot", InstallState::Checking)], true);
        state
            .linux_hooks
            .bindings
            .lock()
            .await
            .insert(stale.pane_id.clone(), stale.clone());
        state
            .linux_hooks
            .jobs
            .lock()
            .await
            .insert(stale.target, Arc::clone(&stale_job));
        super::super::handle_master_wt_event(&state, ready.clone()).await;
        super::super::handle_master_wt_event(&state, ready).await;
        let events = crate::wt_protocol_events::take_test_published_events();
        assert_eq!(events.len(), 2);
        for event in events {
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&event).unwrap()["method"],
                "linux_hooks_discover"
            );
        }
        assert!(state.agents.lock().await.is_empty());
        assert!(state.linux_hooks.jobs.lock().await.is_empty());
        assert!(state.linux_hooks.bindings.lock().await.is_empty());
        assert!(stale_job.cancellation.is_cancelled());
    }

    #[tokio::test]
    async fn closed_panes_cannot_recreate_installation_targets() {
        let state = state(false);
        let binding = binding("tab", "alice");
        register(&state, binding.clone()).await.unwrap();
        pane_closed(&state, &binding.pane_id).await;
        assert!(register(&state, binding.clone()).await.is_err());
        assert!(active_targets(&state).await.is_empty());
        pane_connected(&state, &binding.pane_id).await;
        register(&state, binding).await.unwrap();
    }

    #[tokio::test]
    async fn snapshot_filters_tab_and_provider_without_installation() {
        let state = state(true);
        let own = binding("own", "alice");
        let other = binding("other", "root");
        for binding in [own.clone(), other.clone()] {
            let job = job(
                &[
                    ("copilot", InstallState::Installed),
                    ("claude", InstallState::Disabled),
                ],
                false,
            );
            publish(&job, &*job.work.lock().await);
            state
                .linux_hooks
                .jobs
                .lock()
                .await
                .insert(binding.target.clone(), job);
            state
                .linux_hooks
                .bindings
                .lock()
                .await
                .insert(binding.pane_id.clone(), binding);
        }
        let response = handle_inner(
            &state,
            Request::Snapshot {
                tab_id: "own".into(),
                cli: "copilot".into(),
                pane_id: Some(own.pane_id.clone()),
            },
        )
        .await
        .unwrap();
        assert_eq!(response.targets.len(), 1);
        assert_eq!(response.targets[0].target, own.target);
        assert_eq!(response.targets[0].providers.len(), 1);
        assert_eq!(response.targets[0].providers[0].cli, "copilot");
        assert_eq!(
            response.targets[0].providers[0].status,
            InstallState::Installed
        );
        assert!(
            !state.linux_hooks.jobs.lock().await[&own.target]
                .work
                .lock()
                .await
                .active
        );
    }

    #[tokio::test]
    async fn scoped_retry_does_not_reset_other_providers() {
        let state = state(true);
        let target = binding("tab", "alice").target;
        let job = job(
            &[
                ("copilot", InstallState::Installed),
                ("claude", InstallState::Installed),
            ],
            true,
        );
        state
            .linux_hooks
            .jobs
            .lock()
            .await
            .insert(target.clone(), Arc::clone(&job));
        let _receiver = ensure_install(&state, &target, vec!["copilot".into()], true)
            .await
            .unwrap();
        let work = job.work.lock().await;
        assert_eq!(work.pending, ["copilot".to_owned()].into_iter().collect());
        assert_eq!(work.states["claude"], InstallState::Installed);
        assert_eq!(work.states["copilot"], InstallState::Checking);
    }

    #[tokio::test]
    async fn split_panes_with_different_linux_users_do_not_share_feedback() {
        let state = state(true);
        let alice = binding("same-tab", "alice");
        let root = binding("same-tab", "root");
        for binding in [&alice, &root] {
            let job = job(&[("copilot", InstallState::Disabled)], false);
            publish(&job, &*job.work.lock().await);
            state
                .linux_hooks
                .jobs
                .lock()
                .await
                .insert(binding.target.clone(), job);
            state
                .linux_hooks
                .bindings
                .lock()
                .await
                .insert(binding.pane_id.clone(), binding.clone());
        }
        let ambiguous = handle_inner(
            &state,
            Request::Snapshot {
                tab_id: "same-tab".into(),
                cli: "copilot".into(),
                pane_id: None,
            },
        )
        .await
        .unwrap();
        assert!(ambiguous.targets.is_empty());
        let selected = handle_inner(
            &state,
            Request::Snapshot {
                tab_id: "same-tab".into(),
                cli: "copilot".into(),
                pane_id: Some(alice.pane_id.clone()),
            },
        )
        .await
        .unwrap();
        assert_eq!(selected.targets.len(), 1);
        assert_eq!(selected.targets[0].target, alice.target);
    }

    #[test]
    fn provider_scope_never_overrides_policy() {
        let mut state = state(true);
        Arc::get_mut(&mut state).unwrap().allowed_agent_ids =
            Some(["copilot".to_owned()].into_iter().collect());
        assert_eq!(selected_clis(&state, Some("copilot")).unwrap(), ["copilot"]);
        assert_eq!(selected_clis(&state, None).unwrap(), ["copilot"]);
        assert!(selected_clis(&state, Some("claude")).is_err());
        assert!(selected_clis(&state, Some("custom:example")).is_err());
    }

    #[tokio::test]
    async fn installed_records_wait_for_process_completion_and_manifest_commit() {
        use futures::FutureExt;
        let job = job(&[("copilot", InstallState::Checking)], true);
        let clis = vec!["copilot".to_owned()];
        {
            let mut work = job.work.lock().await;
            record_result(&mut work, "copilot", &InstallState::Installed);
            publish(&job, &work);
        }
        assert!(wait_for(job.changed.subscribe(), &clis)
            .now_or_never()
            .is_none());
        {
            let mut work = job.work.lock().await;
            complete_work(&mut work, &clis, false, false);
            publish(&job, &work);
        }
        assert!(wait_for(job.changed.subscribe(), &clis).await.unwrap()[0]
            .status
            .failed());
    }

    #[test]
    fn partial_install_keeps_verified_providers_but_never_hides_cancellation() {
        let mut work = Work::default();
        let clis = vec!["copilot".to_owned(), "claude".to_owned()];
        record_result(&mut work, "copilot", &InstallState::Installed);
        record_result(
            &mut work,
            "claude",
            &InstallState::Unavailable {
                reason: "install-failed".into(),
            },
        );
        complete_work(&mut work, &clis, false, false);
        assert_eq!(work.states["copilot"], InstallState::Installed);
        assert!(work.states["claude"].failed());
        record_result(&mut work, "copilot", &InstallState::Installed);
        complete_work(&mut work, &["copilot".into()], true, true);
        assert!(work.states["copilot"].failed());
    }

    #[test]
    fn upload_uses_install_only_and_exact_provider_scope() {
        let command = installer_command(42, 54, "nonce", &["copilot".to_owned()]).unwrap();
        assert!(command.contains("--install-only"));
        assert!(command.contains("--report-nonce"));
        assert!(!command.contains("--attach-control"));
        assert!(command.ends_with("'copilot' 'nonce'"));
        assert!(installer_command(0, 1, "nonce", &["copilot".into()]).is_err());
        assert!(installer_command(1, 1, "nonce", &["custom:x".into()]).is_err());
    }
}
