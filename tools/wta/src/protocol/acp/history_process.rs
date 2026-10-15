//! Process containment used only by disposable history ACP servers.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
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

pub(crate) struct HistoryJob(OwnedHandle);

impl HistoryJob {
    pub(crate) fn spawn(
        command: &mut tokio::process::Command,
    ) -> Result<(tokio::process::Child, Self)> {
        // SAFETY: every returned handle is immediately owned. The child is
        // suspended until assigned, so even cmd/npx descendants inherit this
        // job before any agent code can execute.
        let job = unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(std::io::Error::last_os_error()).context("create history process job");
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
                return Err(std::io::Error::last_os_error())
                    .context("configure history process job");
            }
            Self(handle)
        };
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        let mut child = command.spawn().context("spawn contained history server")?;
        let assigned = unsafe {
            // SAFETY: the child and job handles remain live across the call.
            AssignProcessToJobObject(
                job.0.as_raw_handle(),
                child
                    .raw_handle()
                    .ok_or_else(|| anyhow!("history child has no process handle"))?,
            )
        };
        if assigned == 0 {
            let error = std::io::Error::last_os_error();
            if let Err(kill_error) = child.start_kill() {
                tracing::error!(target: "master_history", %kill_error, "failed to kill unassigned suspended history child");
            }
            return Err(error).context("assign history child to job");
        }
        resume_suspended_child(
            child
                .id()
                .ok_or_else(|| anyhow!("history child has no PID"))?,
        )?;
        Ok((child, job))
    }

    pub(crate) async fn terminate(&self) -> Result<()> {
        // SAFETY: this handle refers only to the private history job, never the
        // master, helper, or a chat agent's job.
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
            return Err(std::io::Error::last_os_error()).context("terminate history process job");
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
                    return Err(std::io::Error::last_os_error()).context("query history job exit");
                }
                if accounting.ActiveProcesses == 0 {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .context("history descendants did not exit within 5 seconds")?
    }
}

fn resume_suspended_child(pid: u32) -> Result<()> {
    // SAFETY: snapshots and thread handles are owned; only a thread belonging
    // to the exact still-suspended child is resumed.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error())
                .context("snapshot suspended history thread");
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
                    return Err(std::io::Error::last_os_error())
                        .context("open history primary thread");
                }
                let thread = OwnedHandle::from_raw_handle(thread);
                if ResumeThread(thread.as_raw_handle()) == u32::MAX {
                    return Err(std::io::Error::last_os_error())
                        .context("resume history primary thread");
                }
                return Ok(());
            }
            found = Thread32Next(snapshot.as_raw_handle(), &mut entry);
        }
    }
    Err(anyhow!("suspended history child has no primary thread"))
}

pub(crate) fn wsl_history_script(script: &str, marker: &str) -> String {
    // The group leader stays alive while its stdin proxy forwards ACP bytes.
    // EOF kills the group even when the agent stopped reading its own stdin.
    let inner = format!(
        "IFS= read -r stat < /proc/$$/stat; set -- ${{stat##*) }}; \
         printf '{marker} %s %s\\n' \"$$\" \"${{20}}\"; \
         {script} < <(cat; kill -KILL -- -$$); kill -KILL -- -$$"
    );
    format!(
        "exec setsid --wait bash --noprofile --norc -c {}",
        crate::coordinator::sh_quote(&inner)
    )
}

pub(crate) struct WslHistoryGroup {
    distro: String,
    pid: u32,
    start_ticks: u64,
}

impl WslHistoryGroup {
    pub(crate) fn parse(distro: &str, marker: &str, line: &str) -> Result<Self> {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 3 || fields[0] != marker {
            return Err(anyhow!(
                "WSL history process did not supply its ownership receipt"
            ));
        }
        let pid: u32 = fields[1].parse().context("invalid WSL history group PID")?;
        let start_ticks: u64 = fields[2]
            .parse()
            .context("invalid WSL history start time")?;
        if pid <= 1 || start_ticks == 0 {
            return Err(anyhow!("invalid WSL history process identity"));
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
                let output = command
                    .output()
                    .await
                    .context("launch WSL history cleanup")?;
                if output.status.success() {
                    return Ok(());
                }
                if output.status.code() != Some(6) {
                    return Err(anyhow!(
                        "WSL history group exit could not be confirmed (status {})",
                        output.status
                    ));
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .context("WSL history cleanup timed out")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_ownership_receipt_and_cleanup_reject_unrelated_processes() {
        for line in [
            "other 123 456",
            "marker 1 456",
            "marker 123 0",
            "marker x 456",
            "marker 123 456 extra",
        ] {
            assert!(WslHistoryGroup::parse("Ubuntu", "marker", line).is_err());
        }
        let group = WslHistoryGroup::parse("Ubuntu", "marker", "marker 123 456\n").unwrap();
        let script = group.cleanup_script();
        assert!(script.contains("[ \"${20}\" = '456' ] || exit 3"));
        assert!(script.contains("kill -KILL -- -123"));
        assert!(script.contains("ps -eo pgid="));
        let launch = wsl_history_script("bash -lc 'exec agent' 3>&1 >/dev/null", "marker");
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
        let (mut child, job) = HistoryJob::spawn(&mut command).unwrap();
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
    #[ignore = "requires an explicitly selected WSL distro with bash, setsid, ps and grep"]
    async fn history_wsl_group_reclaims_owned_work_on_recovery_and_host_exit() {
        use tokio::io::AsyncBufReadExt;
        let distro = std::env::var("WTA_TEST_WSL_DISTRO").expect("select WTA_TEST_WSL_DISTRO");
        for host_exit in [false, true] {
            let mut spawned = crate::protocol::acp::spawn::spawn_history_agent_process(
                "sleep 300",
                "custom:history-fixture",
                &crate::agent_source::AgentSource::Wsl {
                    distro: distro.clone(),
                },
                crate::protocol::acp::spawn::SharedProviderSelection::Disabled,
            )
            .unwrap();
            let mut incoming = tokio::io::BufReader::new(spawned.child.stdout.take().unwrap());
            let mut receipt = String::new();
            tokio::time::timeout(Duration::from_secs(20), incoming.read_line(&mut receipt))
                .await
                .unwrap()
                .unwrap();
            let group = WslHistoryGroup::parse(
                &distro,
                spawned.wsl_history_marker.as_ref().unwrap(),
                &receipt,
            )
            .unwrap_or_else(|error| {
                panic!(
                    "{error}: receipt {receipt:?}, status {:?}",
                    spawned.child.try_wait()
                )
            });
            let job = spawned.history_job.take().unwrap();
            if host_exit {
                job.terminate().await.unwrap();
                spawned.child.wait().await.unwrap();
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
                group.terminate().await.unwrap();
                job.terminate().await.unwrap();
                spawned.child.wait().await.unwrap();
            }
        }
    }
}
