//! Native interactive profiles deliberately bypass ACP, helper UI and session synthesis.

mod policy;
#[cfg(test)]
mod tests;

use anyhow::{bail, Context, Result};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::agent_registry::{AgentProfile, KNOWN_AGENTS};

fn profile(id: &str) -> Result<&'static AgentProfile> {
    KNOWN_AGENTS
        .iter()
        .find(|profile| profile.id == id)
        .with_context(|| format!("unknown native agent ID: {id}"))
}

/// Unlike ACP resolution, interactive Claude accepts its npm shim and no
/// provider needs npx or an adapter package. Preserve the registry's extension
/// priority across PATH directories, including runtime-only PATH entries.
fn find_in_path(
    profile: &AgentProfile,
    path: &OsStr,
    is_file: impl Fn(&Path) -> std::io::Result<bool>,
) -> Result<Option<PathBuf>> {
    for extension in profile.exe_search_order {
        for directory in
            std::env::split_paths(path).filter(|directory| !directory.as_os_str().is_empty())
        {
            let candidate = directory.join(format!("{}{extension}", profile.id));
            if is_file(&candidate).context("cannot inspect native agent executable")? {
                return Ok(Some(candidate));
            }
        }
    }
    Ok(None)
}

fn is_file(path: &Path) -> std::io::Result<bool> {
    match path.metadata() {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn effective_path() -> std::ffi::OsString {
    crate::agent_check::spawn_path()
        .map(Into::into)
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default()
}

fn discovery(path: &OsStr, policy: &policy::Policy) -> Result<serde_json::Value> {
    let mut agents = Vec::new();
    for profile in KNOWN_AGENTS
        .iter()
        .filter(|profile| policy.agent_allowed(profile.id))
    {
        if find_in_path(profile, path, is_file)?.is_some() {
            agents
                .push(serde_json::json!({"id": profile.id, "display_name": profile.display_name}));
        }
    }
    Ok(serde_json::json!({"agents": agents}))
}

pub(super) fn probe() -> Result<()> {
    let policy = policy::read()?;
    println!("{}", discovery(&effective_path(), &policy)?);
    Ok(())
}

fn permission_args(id: &str, mode: &str) -> Result<Vec<String>> {
    let args: &[&str] = match (id, mode) {
        (_, "") => &[],
        ("copilot", "allow-all-tools") => &["--allow-all-tools"],
        ("copilot", "allow-all") => &["--allow-all"],
        (
            "claude",
            "acceptEdits" | "auto" | "bypassPermissions" | "manual" | "dontAsk" | "plan",
        ) => &["--permission-mode", mode],
        ("codex", "untrusted" | "on-request" | "never") => &["--ask-for-approval", mode],
        ("gemini", "default" | "auto_edit" | "yolo" | "plan") => &["--approval-mode", mode],
        ("opencode", "auto") => &["--auto"],
        _ => bail!("unsupported --permission-mode for {id}: {mode}"),
    };
    Ok(args.iter().map(|arg| (*arg).to_owned()).collect())
}

/// Permit only reviewed, non-overriding switches. A denylist cannot safely
/// handle provider option abbreviations, config files, short-option clusters,
/// subcommands or future aliases for model/permission overrides.
fn validate_extra_args(id: &str, args: &[String]) -> Result<()> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let (flag, inline_value) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(flag, value)| (flag, Some(value)));
        let takes_value = match (id, flag) {
            (_, "--help" | "--version") => false,
            ("copilot", "--resume" | "--continue") => false,
            ("claude", "--continue") => false,
            ("codex", "--no-alt-screen") => false,
            ("opencode", "--continue") => false,
            ("claude", "--resume" | "--add-dir") => true,
            ("copilot", "--add-dir") => true,
            ("codex", "--add-dir") => true,
            ("gemini", "--resume" | "--prompt-interactive") => true,
            ("opencode", "--session" | "--prompt") => true,
            _ => bail!(
                "unsupported or conflicting additional argument for {id}: {flag}; \
                 use --model and --permission-mode before --"
            ),
        };
        if takes_value {
            let value = inline_value
                .or_else(|| args.next().map(String::as_str))
                .with_context(|| format!("missing value for {flag}"))?;
            if value.is_empty() || value.starts_with('-') {
                bail!("invalid value for additional argument {flag}");
            }
        } else if inline_value.is_some() {
            bail!("unexpected value for additional argument {flag}");
        }
    }
    Ok(())
}

fn launch_args(
    id: &str,
    model: Option<&str>,
    permission_mode: Option<&str>,
    extra: &[String],
) -> Result<Vec<String>> {
    profile(id)?;
    validate_extra_args(id, extra)?;
    let mut args = Vec::new();
    if let Some(model) = model.filter(|value| !value.is_empty()) {
        if model.starts_with('-') {
            bail!("native model identifiers cannot start with '-'");
        }
        args.extend(["--model".to_owned(), model.to_owned()]);
    }
    args.extend(permission_args(id, permission_mode.unwrap_or_default())?);
    args.extend_from_slice(extra);
    if args.iter().any(|arg| arg.contains('\0')) {
        bail!("native arguments cannot contain NUL");
    }
    Ok(args)
}

fn child_command(executable: &Path, args: &[String]) -> Result<tokio::process::Command> {
    // Rust's Windows Command implementation escapes batch arguments and uses
    // cmd.exe /d /c. Do not build a shell command string or use raw_arg here.
    // npm shims forward %*, so reject expansion/line-control characters rather
    // than allowing a second cmd parse to reinterpret them.
    let batch = executable
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cmd"));
    if batch
        && std::iter::once(executable.as_os_str().to_string_lossy().as_ref())
            .chain(args.iter().map(String::as_str))
            .any(|value| value.contains(['%', '!', '\r', '\n', '"', '^']))
    {
        bail!("batch launcher arguments cannot contain %, !, quotes, carets or newlines");
    }
    let mut command = tokio::process::Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    Ok(command)
}

pub(super) async fn launch(
    id: &str,
    model: Option<&str>,
    permission_mode: Option<&str>,
    extra: &[String],
) -> Result<i32> {
    launch_inner(id, model, permission_mode, extra)
        .await
        .with_context(|| t!("setup.subtitle.connection_failed", agent = id).into_owned())
}

async fn launch_inner(
    id: &str,
    model: Option<&str>,
    permission_mode: Option<&str>,
    extra: &[String],
) -> Result<i32> {
    let profile = profile(id)?;
    let policy = policy::read()?;
    policy.check(id)?;
    let args = launch_args(id, model, permission_mode, extra)?;
    let path = effective_path();
    let executable = find_in_path(profile, &path, is_file)?
        .with_context(|| format!("{id}: {}", t!("agent.status.not_found")))?;
    let mut command = child_command(&executable, &args)?;
    command.env("PATH", path);
    install_console_handler()?;
    install_descendant_job()?;
    tracing::info!(target: "native_agent", agent_id = id, "launching native interactive agent");
    let status = command
        .spawn()
        .context("native agent process creation failed")?
        .wait()
        .await
        .context("native agent process wait failed")?;
    let code = status
        .code()
        .context("native agent exit code unavailable")?;
    tracing::info!(target: "native_agent", agent_id = id, exit_code = code, "native agent exited");
    Ok(code)
}

unsafe extern "system" fn console_handler(event: u32) -> i32 {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT};
    // Consume only the wrapper's copy. A non-NULL handler is NOT inherited:
    // the child still receives the original event and controls its own UI.
    i32::from(matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT))
}

fn install_console_handler() -> Result<()> {
    use windows_sys::Win32::Foundation::ERROR_INVALID_HANDLE;
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    // SAFETY: the function has the exact handler ABI and remains valid for the
    // process lifetime. Never use the inheritable NULL-handler ignore flag.
    if unsafe { SetConsoleCtrlHandler(Some(console_handler), 1) } == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_INVALID_HANDLE as i32) {
            return Err(error).context("native console handler registration failed");
        }
    }
    Ok(())
}

fn install_descendant_job() -> Result<()> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    // SAFETY: every handle and pointer is valid for its API call. The handle
    // is non-inheritable. Joining before spawn closes the child-assignment race.
    unsafe {
        let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if handle.is_null() {
            return Err(std::io::Error::last_os_error()).context("native job creation failed");
        }
        let job = OwnedHandle::from_raw_handle(handle);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            std::ptr::addr_of!(limits).cast(),
            std::mem::size_of_val(&limits) as u32,
        ) == 0
        {
            return Err(std::io::Error::last_os_error()).context("native job configuration failed");
        }
        if AssignProcessToJobObject(job.as_raw_handle(), GetCurrentProcess()) == 0 {
            return Err(std::io::Error::last_os_error()).context("native job assignment failed");
        }
        // This wrapper must own the job until process exit, including crashes
        // and TerminateProcess. Closing it here would terminate the wrapper.
        std::mem::forget(job);
    }
    Ok(())
}
