//! Ownership and confirmed cleanup of ACP launchers and their descendants.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::ExitStatus;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation, OpenJobObjectW,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::SystemServices::JOB_OBJECT_QUERY;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenEventW, OpenThread, ResumeThread, SetEvent, WaitForSingleObject,
    CREATE_NO_WINDOW, CREATE_SUSPENDED, EVENT_MODIFY_STATE, SYNCHRONIZATION_SYNCHRONIZE,
    THREAD_SUSPEND_RESUME,
};

pub(crate) const RESOURCE_SCOPE_ENV: &str = "WTA_PROCESS_RESOURCE_SCOPE";
pub(crate) const RESOURCE_SCOPE_RECEIPT_PREFIX: &str = ".retired-";

#[derive(Default, serde::Serialize)]
pub(crate) struct ResourceRecovery {
    pub reclaimed: usize,
    pub deferred: usize,
    pub failed: usize,
}

pub(crate) fn valid_resource_scope(scope: &str) -> bool {
    scope.len() == 32
        && scope
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn request_resource_scope(scope: &str) -> Result<()> {
    anyhow::ensure!(
        valid_resource_scope(scope),
        "managed process resource scope is unavailable"
    );
    let name = |kind| {
        format!("Local\\wta-resources-{kind}-{scope}")
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>()
    };
    let used = name("used");
    let ready = name("ready");
    let job = name("job");
    // SAFETY: names are terminated and each returned handle is immediately owned.
    let open_event = |access, name: &[u16]| -> Result<OwnedHandle> {
        let handle = unsafe { OpenEventW(access, 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error()).context("open managed resource handoff");
        }
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    };
    let used = open_event(EVENT_MODIFY_STATE, &used)?;
    let ready = open_event(SYNCHRONIZATION_SYNCHRONIZE, &ready)?;
    if unsafe { SetEvent(used.as_raw_handle()) } == 0 {
        return Err(std::io::Error::last_os_error()).context("request managed process resources");
    }
    anyhow::ensure!(
        unsafe { WaitForSingleObject(ready.as_raw_handle(), 5000) } == WAIT_OBJECT_0,
        "native parent did not establish managed resource containment"
    );
    let handle = unsafe { OpenJobObjectW(JOB_OBJECT_QUERY, 0, job.as_ptr()) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error()).context("open managed resource owner job");
    }
    let job = unsafe { OwnedHandle::from_raw_handle(handle) };
    let mut member = 0;
    if unsafe { IsProcessInJob(GetCurrentProcess(), job.as_raw_handle(), &mut member) } == 0 {
        return Err(std::io::Error::last_os_error()).context("check managed resource membership");
    }
    anyhow::ensure!(
        member != 0,
        "resource scope does not contain this WTA process"
    );
    Ok(())
}

pub(crate) fn configure_child_error_reporting() {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetErrorMode() -> u32;
        fn SetErrorMode(mode: u32) -> u32;
    }
    // ACP is a noninteractive protocol. Preserve enforcement, but let loaders
    // report failures to stderr instead of waiting on a native error dialog.
    unsafe {
        SetErrorMode(GetErrorMode() | 0x0001);
    }
}

pub(crate) struct ProcessJob(OwnedHandle);

pub(crate) trait ProcessResources: Send + Sync {
    fn job_name(&self) -> &str;
    fn configure(&self, command: &mut tokio::process::Command);
    /// Called for unused resources, after complete job exit, or a spawn that never resumed the child.
    fn cleanup(&mut self) -> Result<()>;
}

/// Owns cleanup until the synchronous handoff to an AgentProcess spawn.
pub(crate) struct PreparedProcessResources(Option<Box<dyn ProcessResources>>);

impl PreparedProcessResources {
    pub(crate) fn new(resources: Box<dyn ProcessResources>) -> Self {
        Self(Some(resources))
    }
}

impl Drop for PreparedProcessResources {
    fn drop(&mut self) {
        if let Some(resources) = self.0.take() {
            cleanup_on_drop(None, resources);
        }
    }
}

impl ProcessJob {
    pub(crate) fn spawn(
        command: &mut tokio::process::Command,
    ) -> Result<(tokio::process::Child, Self)> {
        Self::spawn_named(command, None)
    }

    pub(crate) fn spawn_named(
        command: &mut tokio::process::Command,
        name: Option<&str>,
    ) -> Result<(tokio::process::Child, Self)> {
        let name = name.map(|name| name.encode_utf16().chain(Some(0)).collect::<Vec<_>>());
        // SAFETY: every returned handle is immediately owned. The child is
        // suspended until assigned, so even cmd/npx descendants inherit this
        // job before any agent code can execute.
        let job = unsafe {
            let handle = CreateJobObjectW(
                std::ptr::null(),
                name.as_ref().map_or(std::ptr::null(), |name| name.as_ptr()),
            );
            if handle.is_null() {
                return Err(std::io::Error::last_os_error()).context("create ACP process job");
            }
            let already_exists = GetLastError() == ERROR_ALREADY_EXISTS;
            let handle = OwnedHandle::from_raw_handle(handle);
            anyhow::ensure!(!already_exists, "ACP process job name is already owned");
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            ) == 0
            {
                return Err(std::io::Error::last_os_error()).context("configure ACP process job");
            }
            Self(handle)
        };
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        let mut child = command.spawn().context("spawn contained ACP server")?;
        let assigned = unsafe {
            // SAFETY: the child and job handles remain live across the call.
            AssignProcessToJobObject(
                job.0.as_raw_handle(),
                child
                    .raw_handle()
                    .ok_or_else(|| anyhow!("ACP child has no process handle"))?,
            )
        };
        if assigned == 0 {
            let error = std::io::Error::last_os_error();
            if let Err(kill_error) = child.start_kill() {
                tracing::error!(target: "acp_process", %kill_error, "failed to kill unassigned suspended ACP child");
            }
            return Err(error).context("assign ACP child to job");
        }
        resume_suspended_child(child.id().ok_or_else(|| anyhow!("ACP child has no PID"))?)?;
        Ok((child, job))
    }

    pub(crate) async fn terminate(&self) -> Result<()> {
        self.request_termination()?;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !self.is_empty()? {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Ok(())
        })
        .await
        .context("ACP descendants did not exit within 5 seconds")?
    }

    fn request_termination(&self) -> Result<()> {
        // SAFETY: this handle refers only to this launcher's private job,
        // never the master, helper, or another provider instance.
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
            return Err(std::io::Error::last_os_error()).context("terminate ACP process job");
        }
        Ok(())
    }

    fn terminate_blocking(&self) -> Result<()> {
        self.request_termination()?;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !self.is_empty()? {
            anyhow::ensure!(
                std::time::Instant::now() < deadline,
                "ACP descendants did not exit within 5 seconds"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        Ok(())
    }

    fn is_empty(&self) -> Result<bool> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: the owned job is live and the buffer has the required type and size.
        let queried = unsafe {
            QueryInformationJobObject(
                self.0.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                std::ptr::addr_of_mut!(accounting).cast(),
                std::mem::size_of_val(&accounting) as u32,
                std::ptr::null_mut(),
            )
        };
        if queried == 0 {
            return Err(std::io::Error::last_os_error()).context("query ACP job exit");
        }
        Ok(accounting.ActiveProcesses == 0)
    }

    /// A lost name is not an exit fence: kill-on-close can still be completing.
    /// Only a live handle with zero active processes confirms complete job exit.
    pub(crate) fn named_exit_confirmed(name: &str) -> Result<bool> {
        let name: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
        // SAFETY: name is terminated and the returned handle is immediately owned.
        let handle = unsafe { OpenJobObjectW(JOB_OBJECT_QUERY, 0, name.as_ptr()) };
        if handle.is_null() {
            let error = std::io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) {
                Ok(false)
            } else {
                Err(error).context("open ACP recovery job")
            };
        }
        Self(unsafe { OwnedHandle::from_raw_handle(handle) }).is_empty()
    }
}

pub(crate) struct AgentProcess {
    job: Option<ProcessJob>,
    resources: Option<Box<dyn ProcessResources>>,
    child: tokio::process::Child,
    pub stdin: Option<tokio::process::ChildStdin>,
    pub stderr: Option<tokio::process::ChildStderr>,
    stdout: Option<BufReader<tokio::process::ChildStdout>>,
    wsl_bootstrap: Option<(String, String)>,
    wsl_group: Option<WslProcessGroup>,
    receipt: Vec<u8>,
    stop_requested: tokio_util::sync::CancellationToken,
}

impl AgentProcess {
    pub(crate) fn spawn(
        command: &mut tokio::process::Command,
        wsl_bootstrap: Option<(String, String)>,
    ) -> Result<Self> {
        Self::spawn_with_resources(command, wsl_bootstrap, None)
    }

    pub(crate) fn spawn_with_resources(
        command: &mut tokio::process::Command,
        wsl_bootstrap: Option<(String, String)>,
        resources: Option<PreparedProcessResources>,
    ) -> Result<Self> {
        let resources = resources.and_then(|mut resources| resources.0.take());
        let spawned = match &resources {
            Some(resources) => {
                resources.configure(command);
                ProcessJob::spawn_named(command, Some(resources.job_name()))
            }
            None => ProcessJob::spawn(command),
        };
        let (mut child, job) = match spawned {
            Ok(spawned) => spawned,
            Err(error) => {
                if let Some(resources) = resources {
                    cleanup_on_drop(None, resources);
                }
                return Err(error);
            }
        };
        Ok(Self {
            stdin: child.stdin.take(),
            stderr: child.stderr.take(),
            stdout: child.stdout.take().map(BufReader::new),
            child,
            job: Some(job),
            resources,
            wsl_bootstrap,
            wsl_group: None,
            receipt: Vec::new(),
            stop_requested: tokio_util::sync::CancellationToken::new(),
        })
    }

    pub(crate) fn id(&self) -> Option<u32> {
        self.child.id()
    }

    #[cfg(test)]
    pub(crate) fn raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
        self.child.raw_handle()
    }

    pub(crate) fn stop_token(&self) -> tokio_util::sync::CancellationToken {
        self.stop_requested.clone()
    }

    async fn read_wsl_receipt(&mut self) -> Result<()> {
        if let Some((distro, marker)) = &self.wsl_bootstrap {
            let incoming = self
                .stdout
                .as_mut()
                .ok_or_else(|| anyhow!("WSL ACP bootstrap pipe missing"))?;
            let count = tokio::time::timeout(
                Duration::from_secs(15),
                incoming.read_until(b'\n', &mut self.receipt),
            )
            .await
            .context("WSL ACP ownership receipt timed out")??;
            if count != 0 || !self.receipt.is_empty() {
                self.wsl_group = Some(WslProcessGroup::parse(
                    distro,
                    marker,
                    std::str::from_utf8(&self.receipt)?,
                )?);
            }
            self.wsl_bootstrap = None;
        }
        Ok(())
    }

    pub(crate) async fn take_stdout(&mut self) -> Result<BufReader<tokio::process::ChildStdout>> {
        self.read_wsl_receipt().await?;
        self.stdout.take().context("ACP stdout is not available")
    }

    pub(crate) async fn terminate(&mut self) -> Result<()> {
        let result: Result<()> = async {
            self.stop_requested.cancel();
            self.read_wsl_receipt().await?;
            if let Some(group) = &self.wsl_group {
                group.terminate().await?;
            }
            self.job
                .as_ref()
                .context("ACP process job missing")?
                .terminate()
                .await?;
            tokio::time::timeout(Duration::from_secs(5), self.child.wait())
                .await
                .context("ACP launcher exit timed out")?
                .context("wait for ACP launcher")?;
            if let Some(mut resources) = self.resources.take() {
                let (resources, cleanup) = tokio::task::spawn_blocking(move || {
                    let cleanup = resources.cleanup();
                    (resources, cleanup)
                })
                .await
                .context("ACP runtime cleanup task failed")?;
                if cleanup.is_err() {
                    self.resources = Some(resources);
                }
                cleanup
                    .context(crate::agent_lifecycle::RuntimeCleanupPending)
                    .context("reclaim ACP process resources")?;
            }
            Ok(())
        }
        .await;
        result.context(crate::agent_lifecycle::StartupPhase::Cleanup)
    }

    pub(crate) async fn wait_for_exit_or_stop(&mut self) -> Result<Option<ExitStatus>> {
        let stop = self.stop_requested.clone();
        tokio::select! {
            status = self.child.wait() => status.context("wait for ACP launcher").map(Some),
            _ = stop.cancelled() => Ok(None),
        }
    }

    pub(crate) async fn wait(&mut self) -> Result<ExitStatus> {
        let status = self.wait_for_exit_or_stop().await?;
        self.terminate().await?;
        match status {
            Some(status) => Ok(status),
            None => self
                .child
                .wait()
                .await
                .context("read stopped ACP exit status"),
        }
    }

    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        if let Some(resources) = self.resources.take() {
            cleanup_on_drop(self.job.take(), resources);
        }
    }
}

fn cleanup_on_drop(job: Option<ProcessJob>, mut resources: Box<dyn ProcessResources>) {
    let mut cleanup = move || {
        let result = job
            .as_ref()
            .map_or(Ok(()), ProcessJob::terminate_blocking)
            .and_then(|()| resources.cleanup());
        if let Err(error) = result {
            tracing::warn!(target: "acp_process", error = %format!("{error:#}"),
                "ACP resource cleanup deferred to recovery");
        }
    };
    match tokio::runtime::Handle::try_current() {
        Ok(runtime) => {
            runtime.spawn_blocking(cleanup);
        }
        Err(_) => cleanup(),
    }
}

fn resume_suspended_child(pid: u32) -> Result<()> {
    // SAFETY: snapshots and thread handles are owned; only a thread belonging
    // to the exact still-suspended child is resumed.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error()).context("snapshot suspended ACP thread");
        }
        let snapshot = OwnedHandle::from_raw_handle(snapshot);
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        let mut found = Thread32First(snapshot.as_raw_handle(), &mut entry);
        while found != 0 {
            if entry.th32OwnerProcessID == pid {
                let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                if thread.is_null() {
                    return Err(std::io::Error::last_os_error()).context("open ACP primary thread");
                }
                let thread = OwnedHandle::from_raw_handle(thread);
                if ResumeThread(thread.as_raw_handle()) == u32::MAX {
                    return Err(std::io::Error::last_os_error())
                        .context("resume ACP primary thread");
                }
                return Ok(());
            }
            found = Thread32Next(snapshot.as_raw_handle(), &mut entry);
        }
    }
    Err(anyhow!("suspended ACP child has no primary thread"))
}

pub(crate) fn wsl_process_script(script: &str, marker: &str) -> String {
    // Stderr closure observes host lifetime independently of a blocked stdin
    // relay. Ignore SIGPIPE in the guardian so a failed write reaches cleanup.
    let inner = format!(
        "IFS= read -r stat < /proc/$$/stat; set -- ${{stat##*) }}; \
         printf '{marker} %s %s\\n' \"$$\" \"${{20}}\"; \
         (trap '' PIPE; while sleep 1; do printf '\\n' >&2 || kill -KILL -- -$$; done) & \
         {script} < <(cat; kill -KILL -- -$$); kill -KILL -- -$$"
    );
    format!(
        "exec setsid --wait bash --noprofile --norc -c {}",
        crate::coordinator::sh_quote(&inner)
    )
}

pub(crate) struct WslProcessGroup {
    distro: String,
    pid: u32,
    start_ticks: u64,
}

impl WslProcessGroup {
    pub(crate) fn parse(distro: &str, marker: &str, line: &str) -> Result<Self> {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 3 || fields[0] != marker {
            return Err(anyhow!(
                "WSL ACP process did not supply its ownership receipt"
            ));
        }
        let pid: u32 = fields[1].parse().context("invalid WSL ACP group PID")?;
        let start_ticks: u64 = fields[2].parse().context("invalid WSL ACP start time")?;
        if pid <= 1 || start_ticks == 0 {
            return Err(anyhow!("invalid WSL ACP process identity"));
        }
        Ok(Self {
            distro: distro.to_string(),
            pid,
            start_ticks,
        })
    }

    fn cleanup_script(&self) -> String {
        let pid = self.pid;
        let start = self.start_ticks;
        format!(
            "if [ -e /proc/{pid}/stat ]; then \
             IFS= read -r stat < /proc/{pid}/stat || exit 2; \
             set -- ${{stat##*) }}; [ \"${{20}}\" = '{start}' ] || exit 3; \
             kill -KILL -- -{pid} || exit 4; fi; \
             groups=$(ps -eo pgid=) || exit 5; \
             if printf '%s\\n' \"$groups\" | grep -Eq '^[[:space:]]*{pid}[[:space:]]*$'; then exit 6; fi"
        )
    }

    pub(crate) async fn terminate(&self) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let mut command = tokio::process::Command::new("wsl.exe");
                command.args([
                    "-d",
                    &self.distro,
                    "--exec",
                    "bash",
                    "--noprofile",
                    "--norc",
                    "-c",
                ]);
                command.arg(self.cleanup_script());
                command
                    .stdin(std::process::Stdio::null())
                    .kill_on_drop(true);
                command.creation_flags(CREATE_NO_WINDOW);
                let output = command.output().await.context("launch WSL ACP cleanup")?;
                if output.status.success() {
                    return Ok(());
                }
                if output.status.code() != Some(6) {
                    return Err(anyhow!(
                        "WSL ACP group exit could not be confirmed (status {})",
                        output.status
                    ));
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .context("WSL ACP cleanup timed out")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_resource_scope_rejects_a_process_outside_the_parent_job() {
        use windows_sys::Win32::System::Threading::CreateEventW;
        let scope = uuid::Uuid::new_v4().simple().to_string();
        let name = |kind| {
            format!("Local\\wta-resources-{kind}-{scope}")
                .encode_utf16()
                .chain(Some(0))
                .collect::<Vec<_>>()
        };
        // SAFETY: these are fresh named objects, owned for this test only.
        let used = unsafe { CreateEventW(std::ptr::null(), 1, 0, name("used").as_ptr()) };
        let ready = unsafe { CreateEventW(std::ptr::null(), 1, 1, name("ready").as_ptr()) };
        let job = unsafe { CreateJobObjectW(std::ptr::null(), name("job").as_ptr()) };
        assert!(!used.is_null() && !ready.is_null() && !job.is_null());
        let (_used, _ready, _job) = unsafe {
            (
                OwnedHandle::from_raw_handle(used),
                OwnedHandle::from_raw_handle(ready),
                OwnedHandle::from_raw_handle(job),
            )
        };
        assert!(request_resource_scope(&scope)
            .unwrap_err()
            .to_string()
            .contains("does not contain"));
    }

    #[test]
    fn wsl_ownership_receipt_and_cleanup_reject_unrelated_processes() {
        for line in [
            "other 123 456",
            "marker 1 456",
            "marker 123 0",
            "marker x 456",
            "marker 123 456 extra",
        ] {
            assert!(WslProcessGroup::parse("Ubuntu", "marker", line).is_err());
        }
        let group = WslProcessGroup::parse("Ubuntu", "marker", "marker 123 456\n").unwrap();
        let script = group.cleanup_script();
        assert!(script.contains("[ \"${20}\" = '456' ] || exit 3"));
        assert!(script.contains("kill -KILL -- -123"));
        assert!(script.contains("ps -eo pgid="));
        let launch = wsl_process_script("bash -lc 'exec agent' 3>&1 >/dev/null", "marker");
        assert!(launch.contains("setsid --wait"));
        assert!(launch.contains("printf"));
        assert!(launch.contains("cat; kill -KILL"));
    }

    #[tokio::test]
    async fn history_job_reclaims_launchers_and_their_descendants_only() {
        let mut unrelated = tokio::process::Command::new("pwsh.exe");
        unrelated
            .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 30"])
            .kill_on_drop(true);
        let mut unrelated = unrelated.spawn().unwrap();
        let mut command = tokio::process::Command::new("pwsh.exe");
        command.args(["-NoProfile", "-Command",
            "$child = Start-Process pwsh.exe -ArgumentList '-NoProfile', '-Command', 'Start-Sleep -Seconds 30' -PassThru; Write-Output $child.Id; Start-Sleep -Seconds 30"]);
        command
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true);
        let (mut child, job) = ProcessJob::spawn(&mut command).unwrap();
        use tokio::io::AsyncBufReadExt;
        let mut reader = tokio::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(20), reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(line.trim().parse::<u32>().unwrap() > 0);
        job.terminate().await.unwrap();
        assert!(child.wait().await.unwrap().code().is_some());
        assert!(unrelated.try_wait().unwrap().is_none());
        unrelated.kill().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires an explicitly selected WSL distro with bash, ps and readable process wait channels"]
    async fn wsl_host_exit_reclaims_agent_with_blocked_input() {
        use tokio::io::AsyncWriteExt;

        let distro = std::env::var("WTA_TEST_WSL_DISTRO").expect("select WTA_TEST_WSL_DISTRO");
        let mut spawned = crate::protocol::acp::spawn::spawn_history_agent_process(
            "sleep 300",
            "custom:backpressure-fixture",
            &crate::agent_source::AgentSource::Wsl {
                distro: distro.clone(),
            },
            crate::protocol::acp::spawn::SharedProviderSelection::Disabled,
        )
        .await
        .unwrap();
        let _incoming = spawned.child.take_stdout().await.unwrap();
        let mut input = spawned.child.stdin.take().unwrap();
        let writer = tokio::spawn(async move { input.write_all(&vec![b'x'; 1024 * 1024]).await });
        let group = spawned.child.wsl_group.as_ref().unwrap();
        let observation: Result<()> = async {
            let mut last_state = String::new();
            tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    let script = format!(
                        "while read -r pgid pid comm; do \
                         [ \"$pgid\" = '{}' ] || continue; \
                         [ -r /proc/$pid/wchan ] || continue; channel=$(</proc/$pid/wchan); \
                         printf '%s %s %s\\n' \"$pid\" \"$comm\" \"$channel\"; \
                         case \"$comm:$channel\" in cat:pipe_write|cat:anon_pipe_write) exit 0 ;; esac; \
                         done < <(ps -eo pgid=,pid=,comm=); exit 1",
                        group.pid
                    );
                    let output = tokio::process::Command::new("wsl.exe")
                        .args(["-d", &distro, "--exec", "bash", "--noprofile", "--norc", "-c", &script])
                        .stdin(std::process::Stdio::null())
                        .kill_on_drop(true)
                        .output()
                        .await?;
                    last_state = String::from_utf8_lossy(&output.stdout).into_owned();
                    if output.status.success() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Ok::<_, std::io::Error>(())
            })
            .await
            .with_context(|| format!("fixture did not fill the owned forwarding pipe: {last_state}"))??;

            spawned.child.job.as_ref().unwrap().terminate().await?;
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let script = format!(
                        "groups=$(ps -eo pgid=) || exit 2; ! printf '%s\\n' \"$groups\" | grep -Eq '^[[:space:]]*{}[[:space:]]*$'",
                        group.pid
                    );
                    let status = tokio::process::Command::new("wsl.exe")
                        .args(["-d", &distro, "--exec", "bash", "--noprofile", "--norc", "-c", &script])
                        .stdin(std::process::Stdio::null())
                        .kill_on_drop(true)
                        .status()
                        .await?;
                    if status.success() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Ok::<_, std::io::Error>(())
            })
            .await
            .context("host exit must reclaim a group with a full forwarding pipe")??;
            Ok(())
        }
        .await;
        writer.abort();
        let _ = writer.await;
        group
            .terminate()
            .await
            .expect("reclaim this test's owned group");
        observation.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires an explicitly selected WSL distro with bash, setsid, ps and grep"]
    async fn history_wsl_group_reclaims_owned_work_on_recovery_and_host_exit() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        let distro = std::env::var("WTA_TEST_WSL_DISTRO").expect("select WTA_TEST_WSL_DISTRO");
        for host_exit in [false, true] {
            let mut spawned = crate::protocol::acp::spawn::spawn_history_agent_process(
                if host_exit { "sleep 300" } else { "cat" },
                "custom:history-fixture",
                &crate::agent_source::AgentSource::Wsl {
                    distro: distro.clone(),
                },
                crate::protocol::acp::spawn::SharedProviderSelection::Disabled,
            )
            .await
            .unwrap();
            let mut incoming = spawned.child.take_stdout().await.unwrap();
            let group = spawned.child.wsl_group.as_ref().unwrap();
            if host_exit {
                spawned
                    .child
                    .job
                    .as_ref()
                    .unwrap()
                    .terminate()
                    .await
                    .unwrap();
                let gone = tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let script = format!("groups=$(ps -eo pgid=) || exit 2; ! printf '%s\\n' \"$groups\" | grep -Eq '^[[:space:]]*{}[[:space:]]*$'", group.pid);
                        let status = tokio::process::Command::new("wsl.exe")
                            .args(["-d", &distro,                             "--exec", "bash", "--noprofile", "--norc", "-c", &script])
                            .stdin(std::process::Stdio::null()).kill_on_drop(true).status().await.unwrap();
                        if status.success() { break; }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }).await;
                group.terminate().await.unwrap();
                assert!(gone.is_ok(), "stdin EOF guardian must kill Linux work even if the Windows launcher is killed");
            } else {
                let mut stderr = BufReader::new(spawned.child.stderr.take().unwrap());
                for _ in 0..2 {
                    let mut heartbeat = String::new();
                    tokio::time::timeout(Duration::from_secs(5), stderr.read_line(&mut heartbeat))
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(heartbeat, "\n");
                }
                let payload = "owned transport remains usable\n";
                spawned
                    .child
                    .stdin
                    .as_mut()
                    .unwrap()
                    .write_all(payload.as_bytes())
                    .await
                    .unwrap();
                let mut echoed = String::new();
                tokio::time::timeout(Duration::from_secs(5), incoming.read_line(&mut echoed))
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    echoed, payload,
                    "lifetime signaling must not alter ACP stdout"
                );
                assert!(spawned.child.try_wait().unwrap().is_none());
                spawned.child.terminate().await.unwrap();
            }
        }
    }
}
