//! Ownership and confirmed cleanup of ACP launchers and their descendants.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::ExitStatus;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    OpenThread, ResumeThread, CREATE_NO_WINDOW, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME,
};

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

impl AsRawHandle for ProcessJob {
    fn as_raw_handle(&self) -> std::os::windows::io::RawHandle {
        self.0.as_raw_handle()
    }
}

impl ProcessJob {
    pub(super) fn new() -> Result<Self> {
        // SAFETY: every successful handle is immediately owned.
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(std::io::Error::last_os_error()).context("create ACP process job");
            }
            let handle = OwnedHandle::from_raw_handle(handle);
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
            Ok(Self(handle))
        }
    }

    pub(super) fn assign(&self, process: std::os::windows::io::RawHandle) -> Result<()> {
        // SAFETY: callers retain the still-suspended child's process handle.
        if unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), process) } == 0 {
            return Err(std::io::Error::last_os_error()).context("assign ACP child to job");
        }
        Ok(())
    }

    pub(crate) fn spawn(
        command: &mut tokio::process::Command,
    ) -> Result<(tokio::process::Child, Self)> {
        let job = Self::new()?;
        // Assignment precedes resume, including for cmd/npx descendants.
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        let mut child = command.spawn().context("spawn contained ACP server")?;
        if let Err(error) = job.assign(
            child
                .raw_handle()
                .ok_or_else(|| anyhow!("ACP child has no process handle"))?,
        ) {
            if let Err(kill_error) = child.start_kill() {
                tracing::error!(target: "acp_process", %kill_error, "failed to kill unassigned suspended ACP child");
            }
            return Err(error);
        }
        resume_suspended_child(child.id().ok_or_else(|| anyhow!("ACP child has no PID"))?)?;
        Ok((child, job))
    }

    pub(crate) async fn terminate(&self) -> Result<()> {
        // SAFETY: this handle refers only to this launcher's private job,
        // never the master, helper, or another provider instance.
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
            return Err(std::io::Error::last_os_error()).context("terminate ACP process job");
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
                let queried = unsafe {
                    // SAFETY: accounting has the exact size/type required by
                    // JobObjectBasicAccountingInformation.
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
                if accounting.ActiveProcesses == 0 {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .context("ACP descendants did not exit within 5 seconds")?
    }
}

pub(crate) struct AgentProcess {
    job: ProcessJob,
    child: tokio::process::Child,
    pub stdin: Option<tokio::process::ChildStdin>,
    pub stderr: Option<tokio::process::ChildStderr>,
    stdout: Option<BufReader<tokio::process::ChildStdout>>,
    wsl_bootstrap: Option<(String, String)>,
    wsl_group: Option<WslProcessGroup>,
    receipt: Vec<u8>,
    stop_requested: tokio_util::sync::CancellationToken,
    graceful_shutdown: Option<super::console_host::ShutdownRequest>,
    completion: Option<tokio::sync::watch::Sender<Option<bool>>>,
}

#[derive(Clone)]
pub(crate) struct GracefulShutdown {
    stop: tokio_util::sync::CancellationToken,
    completion: tokio::sync::watch::Receiver<Option<bool>>,
    pub grace: Duration,
}

impl GracefulShutdown {
    pub fn request(&self) {
        self.stop.cancel();
    }

    pub async fn wait(mut self) -> Result<()> {
        let completed = *self
            .completion
            .wait_for(Option::is_some)
            .await
            .context("ACP process owner ended before confirming exit")?;
        if completed != Some(true) {
            return Err(anyhow!("ACP process cleanup did not confirm exit"));
        }
        Ok(())
    }
}

impl AgentProcess {
    pub(crate) fn spawn(
        command: &mut tokio::process::Command,
        wsl_bootstrap: Option<(String, String)>,
    ) -> Result<Self> {
        Self::spawn_with_graceful_shutdown(command, wsl_bootstrap, None)
    }

    pub(crate) fn spawn_with_graceful_shutdown(
        command: &mut tokio::process::Command,
        wsl_bootstrap: Option<(String, String)>,
        graceful_shutdown: Option<super::console_host::ShutdownRequest>,
    ) -> Result<Self> {
        let (mut child, job) = ProcessJob::spawn(command)?;
        let completion = graceful_shutdown
            .as_ref()
            .map(|_| tokio::sync::watch::channel(None).0);
        Ok(Self {
            stdin: child.stdin.take(),
            stderr: child.stderr.take(),
            stdout: child.stdout.take().map(BufReader::new),
            child,
            job,
            wsl_bootstrap,
            wsl_group: None,
            receipt: Vec::new(),
            stop_requested: tokio_util::sync::CancellationToken::new(),
            graceful_shutdown,
            completion,
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

    pub(crate) fn graceful_shutdown(&self) -> Option<GracefulShutdown> {
        Some(GracefulShutdown {
            stop: self.stop_requested.clone(),
            completion: self.completion.as_ref()?.subscribe(),
            grace: self.graceful_shutdown.as_ref()?.grace,
        })
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
            if let Some(shutdown) = &self.graceful_shutdown {
                let deadline = tokio::time::Instant::now() + shutdown.grace;
                match shutdown.request() {
                    Ok(()) => match tokio::time::timeout_at(deadline, self.child.wait()).await {
                        Ok(Ok(_)) => {}
                        Ok(Err(error)) => tracing::warn!(target: "acp_process", %error,
                            "native ACP exit wait failed; using contained teardown"),
                        Err(_) => tracing::warn!(target: "acp_process",
                            "native ACP interrupt deadline reached; using contained teardown"),
                    },
                    Err(error) => tracing::warn!(target: "acp_process", %error,
                        "native ACP interrupt request failed; using contained teardown"),
                }
            }
            self.job.terminate().await?;
            tokio::time::timeout(Duration::from_secs(5), self.child.wait())
                .await
                .context("ACP launcher exit timed out")?
                .context("wait for ACP launcher")?;
            Ok(())
        }
        .await;
        if let Some(completion) = &self.completion {
            let _ = completion.send_replace(Some(result.is_ok()));
        }
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
pub(crate) mod tests {
    use super::*;

    const NATIVE_EXIT_FIXTURE: &str = "protocol::acp::process::tests::native_exit_fixture";
    const NATIVE_EXIT_CONFIGURATION: &str = "WTA_TEST_NATIVE_EXIT";

    pub(crate) fn graceful_shutdown_fixture(
        grace: Duration,
    ) -> (
        GracefulShutdown,
        tokio_util::sync::CancellationToken,
        tokio::sync::watch::Sender<Option<bool>>,
    ) {
        let stop = tokio_util::sync::CancellationToken::new();
        let (sender, completion) = tokio::sync::watch::channel(None);
        (
            GracefulShutdown {
                stop: stop.clone(),
                completion,
                grace,
            },
            stop,
            sender,
        )
    }

    #[test]
    fn native_exit_fixture() {
        use std::io::Write;
        use std::sync::atomic::{AtomicBool, Ordering};
        use windows_sys::Win32::System::Console::{
            GetConsoleWindow, SetConsoleCtrlHandler, CTRL_BREAK_EVENT,
        };

        // Environment is briefly configured in the test runner during spawn;
        // only the explicitly selected child may enter the fixture loop.
        let arguments: Vec<_> = std::env::args().collect();
        if !arguments
            .windows(2)
            .any(|pair| pair == ["--exact", NATIVE_EXIT_FIXTURE])
        {
            return;
        }
        let Ok(configuration) = std::env::var(NATIVE_EXIT_CONFIGURATION) else {
            return;
        };
        let configuration: serde_json::Value = serde_json::from_str(&configuration).unwrap();
        for key in ["TEMP", "TMP"] {
            assert_eq!(
                serde_json::to_value(std::env::var(key).ok()).unwrap(),
                configuration[key],
                "native temporary environment must remain inherited"
            );
        }
        assert_eq!(
            std::env::current_dir().unwrap(),
            std::path::PathBuf::from(configuration["cwd"].as_str().unwrap())
        );
        static INTERRUPTED: AtomicBool = AtomicBool::new(false);
        unsafe extern "system" fn on_control(event: u32) -> i32 {
            if event == CTRL_BREAK_EVENT {
                INTERRUPTED.store(true, Ordering::SeqCst);
                return 1;
            }
            0
        }
        // The baseline has no console; it is ready but cannot receive a
        // native interrupt. Only the opted-in launch supplies this channel.
        if !unsafe { GetConsoleWindow() }.is_null() {
            assert_ne!(unsafe { SetConsoleCtrlHandler(Some(on_control), 1) }, 0);
        }
        println!("NATIVE_EXIT_READY");
        std::io::stdout().flush().unwrap();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).unwrap();
        print!("{input}");
        std::io::stdout().flush().unwrap();
        loop {
            if configuration["cooperative"] == true && INTERRUPTED.load(Ordering::SeqCst) {
                std::fs::write(configuration["marker"].as_str().unwrap(), b"native cleanup")
                    .unwrap();
                std::process::exit(0);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub(crate) async fn native_exit_test_process(
        agent_id: &str,
        marker: &std::path::Path,
        cooperative: bool,
    ) -> AgentProcess {
        use crate::protocol::acp::spawn::{
            spawn_agent_process_with_provider, ChildEnvironmentPolicy, SharedProviderSelection,
        };

        let spawned = {
            let _environment = crate::test_support::lock_env();
            let previous = std::env::var_os(NATIVE_EXIT_CONFIGURATION);
            std::env::set_var(
                NATIVE_EXIT_CONFIGURATION,
                serde_json::json!({
                    "marker": marker,
                    "cooperative": cooperative,
                    "TEMP": std::env::var("TEMP").ok(),
                    "TMP": std::env::var("TMP").ok(),
                    "cwd": std::env::current_dir().unwrap(),
                })
                .to_string(),
            );
            let command = format!(
                "{} --exact {NATIVE_EXIT_FIXTURE} --nocapture",
                std::env::current_exe().unwrap().display()
            );
            let spawned = spawn_agent_process_with_provider(
                &command,
                None,
                Some(agent_id),
                ChildEnvironmentPolicy::ApplySharedProvider,
                SharedProviderSelection::Disabled,
            );
            match previous {
                Some(value) => std::env::set_var(NATIVE_EXIT_CONFIGURATION, value),
                None => std::env::remove_var(NATIVE_EXIT_CONFIGURATION),
            }
            spawned.unwrap()
        };
        let mut process = spawned.child;
        let mut output = process.take_stdout().await.unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let mut line = String::new();
                assert_ne!(output.read_line(&mut line).await.unwrap(), 0);
                if line.trim() == "NATIVE_EXIT_READY" {
                    break;
                }
            }
            use tokio::io::AsyncWriteExt;
            let payload = "native stdio remains unchanged\n";
            process
                .stdin
                .as_mut()
                .unwrap()
                .write_all(payload.as_bytes())
                .await
                .unwrap();
            let mut echoed = String::new();
            output.read_line(&mut echoed).await.unwrap();
            assert_eq!(echoed, payload);
        })
        .await
        .expect("the owned native fixture must initialize before testing shutdown");
        process
    }

    #[tokio::test]
    async fn antigravity_native_cleanup_runs_before_forced_job_fallback() {
        let directory =
            std::env::temp_dir().join(format!("wta-native-exit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let marker = directory.join("native-cleanup");
        let mut custom =
            native_exit_test_process("custom:native-exit-fixture", &marker, true).await;
        custom.terminate().await.unwrap();
        assert!(
            !marker.exists(),
            "custom commands must keep their existing forceful teardown"
        );

        let mut native = native_exit_test_process("antigravity", &marker, true).await;
        native.terminate().await.unwrap();
        let cleaned = marker.is_file();
        if cleaned {
            std::fs::remove_file(&marker).unwrap();
        }
        std::fs::remove_dir(&directory).unwrap();
        assert!(
            cleaned,
            "the Antigravity opt-in must let the process perform native cleanup before containment teardown"
        );
    }

    #[tokio::test]
    async fn antigravity_native_cleanup_timeout_preserves_containment_and_other_processes() {
        let directory =
            std::env::temp_dir().join(format!("wta-native-deadline-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let marker = directory.join("native-cleanup");
        let mut unrelated = native_exit_test_process("custom:unrelated", &marker, true).await;
        let mut native = native_exit_test_process("antigravity", &marker, false).await;
        native.graceful_shutdown.as_mut().unwrap().grace = Duration::from_millis(100);
        tokio::time::timeout(Duration::from_secs(3), native.terminate())
            .await
            .expect("non-cooperation must reach the existing contained fallback")
            .unwrap();
        assert!(native.try_wait().unwrap().is_some());
        assert!(
            !marker.exists(),
            "forced fallback must not claim native cleanup"
        );
        assert!(unrelated.try_wait().unwrap().is_none());
        unrelated.terminate().await.unwrap();
        std::fs::remove_dir(directory).unwrap();
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

            spawned.child.job.terminate().await?;
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
            .unwrap();
            let mut incoming = spawned.child.take_stdout().await.unwrap();
            let group = spawned.child.wsl_group.as_ref().unwrap();
            if host_exit {
                spawned.child.job.terminate().await.unwrap();
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
