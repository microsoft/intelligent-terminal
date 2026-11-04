//! Opt-in, contained console interruption for stdio processes.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use windows_sys::Win32::Foundation::{
    DuplicateHandle, DUPLICATE_SAME_ACCESS, ERROR_ALREADY_EXISTS, HANDLE, INVALID_HANDLE_VALUE,
    WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Console::{
    AttachConsole, FreeConsole, GenerateConsoleCtrlEvent, GetConsoleProcessList, GetStdHandle,
    SetConsoleCtrlHandler, CTRL_BREAK_EVENT, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess,
    GetCurrentProcessId, GetExitCodeProcess, InitializeProcThreadAttributeList, OpenEventW,
    OpenProcess, ResumeThread, SetEvent, TerminateProcess, UpdateProcThreadAttribute,
    WaitForMultipleObjects, WaitForSingleObject, CREATE_NEW_CONSOLE, CREATE_SUSPENDED,
    EXTENDED_STARTUPINFO_PRESENT, INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTF_USESHOWWINDOW,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, SYNCHRONIZATION_SYNCHRONIZE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

use super::process::ProcessJob;

pub(crate) struct ShutdownRequest {
    event: OwnedHandle,
    pub grace: Duration,
}

impl ShutdownRequest {
    pub fn request(&self) -> Result<()> {
        // SAFETY: this instance owns the event created for its console host.
        if unsafe { SetEvent(self.event.as_raw_handle()) } == 0 {
            return Err(std::io::Error::last_os_error()).context("request native ACP interrupt");
        }
        Ok(())
    }
}

pub(crate) fn command(
    program: &str,
    arguments: &[&str],
    grace: Duration,
) -> Result<(tokio::process::Command, ShutdownRequest)> {
    let name = format!("Local\\wta-acp-stop-{}", uuid::Uuid::new_v4());
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: the name is terminated; only the successful handle is adopted.
    let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, wide.as_ptr()) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error()).context("create native ACP stop event");
    }
    let error = std::io::Error::last_os_error();
    let event = unsafe { OwnedHandle::from_raw_handle(handle) };
    if error.raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32) {
        bail!("native ACP stop event already exists");
    }
    let mut command =
        tokio::process::Command::new(std::env::current_exe().context("resolve ACP console host")?);
    #[cfg(not(test))]
    command
        .args(["acp-console-host", "--stop-event", &name, "--", program])
        .args(arguments);
    #[cfg(test)]
    command
        .args([
            "--exact",
            "protocol::acp::console_host::tests::console_host_fixture",
            "--nocapture",
        ])
        .env(
            "WTA_TEST_CONSOLE_HOST",
            serde_json::json!({
                "event": name,
                "arguments": std::iter::once(program).chain(arguments.iter().copied()).collect::<Vec<_>>(),
            })
            .to_string(),
        );
    Ok((command, ShutdownRequest { event, grace }))
}

struct HandleList {
    storage: Vec<usize>,
}

impl HandleList {
    fn new(handles: &mut [HANDLE]) -> Result<Self> {
        let mut bytes = 0;
        // SAFETY: the first call obtains the required allocation size.
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut bytes);
        }
        if bytes == 0 {
            return Err(std::io::Error::last_os_error()).context("size ACP inherited-handle list");
        }
        let mut storage = vec![0usize; bytes.div_ceil(std::mem::size_of::<usize>())];
        // SAFETY: the aligned storage has the size reported by Windows.
        if unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), 1, 0, &mut bytes)
        } == 0
        {
            return Err(std::io::Error::last_os_error())
                .context("initialize ACP inherited-handle list");
        }
        let mut list = Self { storage };
        if unsafe {
            UpdateProcThreadAttribute(
                list.pointer(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_mut_ptr().cast(),
                std::mem::size_of_val(handles),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error()).context("restrict ACP inherited handles");
        }
        Ok(list)
    }

    fn pointer(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }
}

impl Drop for HandleList {
    fn drop(&mut self) {
        // SAFETY: storage outlives the initialized attribute list.
        unsafe { DeleteProcThreadAttributeList(self.pointer()) };
    }
}

fn inherit_standard_handle(which: u32) -> Result<OwnedHandle> {
    // SAFETY: duplicate only an existing redirected standard handle.
    unsafe {
        let original = GetStdHandle(which);
        if original.is_null() || original == INVALID_HANDLE_VALUE {
            bail!("ACP console host has no redirected standard handle");
        }
        let mut copy = std::ptr::null_mut();
        if DuplicateHandle(
            GetCurrentProcess(),
            original,
            GetCurrentProcess(),
            &mut copy,
            0,
            1,
            DUPLICATE_SAME_ACCESS,
        ) == 0
        {
            return Err(std::io::Error::last_os_error()).context("duplicate ACP stdio handle");
        }
        Ok(OwnedHandle::from_raw_handle(copy))
    }
}

unsafe extern "system" fn ignore_break(event: u32) -> i32 {
    i32::from(event == CTRL_BREAK_EVENT)
}

fn interrupt_private_console(job: &ProcessJob, process: &OwnedHandle, pid: u32) -> Result<()> {
    // SAFETY: console state changes affect only this dedicated host process,
    // not the master/helper or any other provider.
    unsafe {
        if WaitForSingleObject(process.as_raw_handle(), 0) == WAIT_OBJECT_0 {
            return Ok(());
        }
        if FreeConsole() == 0 || AttachConsole(pid) == 0 {
            return Err(std::io::Error::last_os_error()).context("attach owned ACP console");
        }
        let mut members = vec![0; 16];
        let mut count = GetConsoleProcessList(members.as_mut_ptr(), members.len() as u32);
        if count as usize > members.len() {
            members.resize(count as usize, 0);
            count = GetConsoleProcessList(members.as_mut_ptr(), members.len() as u32);
        }
        if count == 0 || count as usize > members.len() {
            bail!("cannot establish owned ACP console membership");
        }
        for &member in &members[..count as usize] {
            if member == GetCurrentProcessId() {
                continue;
            }
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, member);
            if handle.is_null() {
                return Err(std::io::Error::last_os_error()).context("inspect ACP console member");
            }
            let member = OwnedHandle::from_raw_handle(handle);
            let mut owned = 0;
            if IsProcessInJob(member.as_raw_handle(), job.as_raw_handle(), &mut owned) == 0 {
                return Err(std::io::Error::last_os_error()).context("check ACP console ownership");
            }
            if owned == 0 {
                bail!("refusing to interrupt a console containing an unrelated process");
            }
        }
        if SetConsoleCtrlHandler(Some(ignore_break), 1) == 0 {
            return Err(std::io::Error::last_os_error()).context("protect ACP console host");
        }
        if WaitForSingleObject(process.as_raw_handle(), 0) != WAIT_OBJECT_0
            && GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, 0) == 0
        {
            return Err(std::io::Error::last_os_error()).context("interrupt owned ACP console");
        }
        // Stay attached: detaching resets handlers before the asynchronous
        // event has necessarily reached this sender.
    }
    Ok(())
}

pub(crate) fn run(event_name: &str, arguments: &[String]) -> Result<i32> {
    if event_name.contains('\0')
        || arguments.first().is_none_or(String::is_empty)
        || arguments.iter().any(|a| a.contains('\0'))
    {
        bail!("invalid ACP console host invocation");
    }
    let event_name: Vec<u16> = event_name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: the host opens, but cannot signal, the parent's existing event.
    let event = unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, 0, event_name.as_ptr()) };
    if event.is_null() {
        return Err(std::io::Error::last_os_error()).context("open native ACP stop event");
    }
    let event = unsafe { OwnedHandle::from_raw_handle(event) };
    let inherited = [
        inherit_standard_handle(STD_INPUT_HANDLE)?,
        inherit_standard_handle(STD_OUTPUT_HANDLE)?,
        inherit_standard_handle(STD_ERROR_HANDLE)?,
    ];
    let mut handles = inherited.each_ref().map(AsRawHandle::as_raw_handle);
    let mut attributes = HandleList::new(&mut handles)?;
    let job = ProcessJob::new()?;
    let argv: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let mut command: Vec<u16> = crate::coordinator::join_windows_commandline(&argv)
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = std::mem::size_of_val(&startup) as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
    startup.StartupInfo.wShowWindow = SW_HIDE as u16;
    startup.StartupInfo.hStdInput = handles[0];
    startup.StartupInfo.hStdOutput = handles[1];
    startup.StartupInfo.hStdError = handles[2];
    startup.lpAttributeList = attributes.pointer();
    let mut child = PROCESS_INFORMATION::default();
    // SAFETY: buffers, attribute list and restricted inheritable handles
    // remain live. Null environment/cwd preserve the normal launch inputs.
    // The encoded argv[0] permits normal PATH lookup for bare executable names.
    if unsafe {
        CreateProcessW(
            std::ptr::null(),
            command.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            CREATE_NEW_CONSOLE | CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT,
            std::ptr::null(),
            std::ptr::null(),
            &startup.StartupInfo,
            &mut child,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error()).context("launch hidden native ACP console");
    }
    let process = unsafe { OwnedHandle::from_raw_handle(child.hProcess) };
    let thread = unsafe { OwnedHandle::from_raw_handle(child.hThread) };
    if let Err(error) = job.assign(process.as_raw_handle()) {
        if unsafe { TerminateProcess(process.as_raw_handle(), 1) } == 0 {
            tracing::error!(target: "acp_process", error = %std::io::Error::last_os_error(),
                "failed to kill unassigned native ACP child");
        }
        return Err(error);
    }
    if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
        return Err(std::io::Error::last_os_error()).context("resume native ACP child");
    }
    drop(thread);
    drop(attributes);
    drop(inherited);
    let waits = [process.as_raw_handle(), event.as_raw_handle()];
    // This dedicated host remains contained by both jobs. The owner enforces
    // the provider's deadline and kills the entire tree if interruption fails.
    match unsafe { WaitForMultipleObjects(waits.len() as u32, waits.as_ptr(), 0, INFINITE) } {
        WAIT_OBJECT_0 => {}
        result if result == WAIT_OBJECT_0 + 1 => {
            interrupt_private_console(&job, &process, child.dwProcessId)?;
            if unsafe { WaitForSingleObject(process.as_raw_handle(), INFINITE) } != WAIT_OBJECT_0 {
                return Err(std::io::Error::last_os_error()).context("wait for native ACP exit");
            }
        }
        _ => return Err(std::io::Error::last_os_error()).context("wait for native ACP stop"),
    }
    let mut status = 0;
    if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut status) } == 0 {
        return Err(std::io::Error::last_os_error()).context("read native ACP exit status");
    }
    Ok(status as i32)
}

#[cfg(test)]
mod tests {
    #[test]
    fn console_host_fixture() {
        let Ok(value) = std::env::var("WTA_TEST_CONSOLE_HOST") else {
            return;
        };
        let value: serde_json::Value = serde_json::from_str(&value).unwrap();
        let arguments: Vec<String> = serde_json::from_value(value["arguments"].clone()).unwrap();
        let code = super::run(value["event"].as_str().unwrap(), &arguments).unwrap();
        std::process::exit(code);
    }
}
