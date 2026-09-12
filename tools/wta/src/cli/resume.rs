use std::io::Write;
use std::path::Path;

use anyhow::{bail, Context, Result};

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ResumeLaunch {
    pub agent: String,
    pub session_id: String,
    pub cwd: Option<String>,
    pub distro: Option<String>,
}

impl ResumeLaunch {
    fn validate(&self) -> Result<()> {
        let profile = crate::agent_registry::lookup_profile_by_id(&self.agent);
        if profile.id != self.agent || profile.resume_flag.is_empty() {
            bail!("resume launcher does not recognize this agent or its resume syntax");
        }
        if self.session_id.contains('\0') {
            bail!("session identifier contains NUL, which cannot be represented in a process argument");
        }
        Ok(())
    }

    fn encode(&self) -> Result<String> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).context("serialize resume launch data")?;
        Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    fn decode(payload: &str) -> Result<Self> {
        if payload.len() % 2 != 0 || !payload.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("invalid encoded resume launch data");
        }
        let bytes = payload
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let digits =
                    std::str::from_utf8(pair).context("invalid resume payload encoding")?;
                u8::from_str_radix(digits, 16).context("invalid resume payload digit")
            })
            .collect::<Result<Vec<_>>>()?;
        let plan: Self = serde_json::from_slice(&bytes).context("decode resume launch data")?;
        plan.validate()?;
        Ok(plan)
    }

    pub(crate) fn banner(&self) -> String {
        let short: String = self.session_id.chars().take(8).collect();
        let suffix = self
            .distro
            .as_ref()
            .map(|distro| format!(" in {distro} (WSL)"))
            .unwrap_or_default();
        let text = format!("Resuming {} session {short}{suffix}...", self.agent);
        let text: String = text
            .chars()
            .map(|ch| {
                if ch.is_control() {
                    ch.escape_default().to_string()
                } else {
                    ch.to_string()
                }
            })
            .collect();
        format!("\x1b[2;37m{text}\x1b[0m")
    }

    fn host_command(&self, executable: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
        let mut command = std::process::Command::new(executable);
        // Rust's Windows batch-file handling uses cmd-specific escaping,
        // disables delayed expansion, and rejects unrepresentable batch args.
        // Do not replace this with cmd /c, raw_arg, or PowerShell batch invocation.
        command.args([
            crate::agent_registry::lookup_profile_by_id(&self.agent).resume_flag,
            self.session_id.as_str(),
        ]);
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        command
    }

    fn wsl_args(&self) -> Vec<String> {
        let mut args = vec![
            "--distribution".into(),
            self.distro.clone().unwrap_or_default(),
        ];
        if let Some(cwd) = &self.cwd {
            args.extend(["--cd".into(), cwd.clone()]);
        }
        // --exec bypasses WSL's intermediate default-shell command string.
        // The login shell sees only a fixed script; all data is positional argv.
        args.extend([
            "--exec".into(),
            "bash".into(),
            "-lc".into(),
            "exec \"$@\"".into(),
            "wta-resume".into(),
            self.agent.clone(),
            crate::agent_registry::lookup_profile_by_id(&self.agent)
                .resume_flag
                .into(),
            self.session_id.clone(),
        ]);
        args
    }
}

pub(crate) fn commandline(plan: &ResumeLaunch) -> Result<String> {
    let exe = std::env::current_exe().context("resolve WTA resume launcher executable")?;
    bootstrap_commandline(&exe, &plan.encode()?)
}

fn bootstrap_commandline(exe: &Path, payload: &str) -> Result<String> {
    use std::os::windows::ffi::OsStrExt;
    let path_bytes: Vec<_> = exe
        .as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect();
    let path = crate::osc52::base64_encode(&path_bytes);
    let script = format!(
        "$ErrorActionPreference='Stop';$exe=[Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('{path}'));& $exe resume-session {payload};exit $LASTEXITCODE"
    );
    let bytes: Vec<_> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    // The entire transport is inert ASCII, including the executable path:
    // no opaque data is exposed to WT's ExpandEnvironmentStringsW.
    Ok(format!(
        "powershell.exe -NoLogo -NoProfile -EncodedCommand {}",
        crate::osc52::base64_encode(&bytes)
    ))
}

pub(crate) async fn run(payload: &str) -> Result<()> {
    let plan = ResumeLaunch::decode(payload)?;
    writeln!(std::io::stdout(), "{}", plan.banner()).context("write resume startup banner")?;
    std::io::stdout()
        .flush()
        .context("flush resume startup banner")?;
    let command = if plan.distro.is_some() {
        let mut command = std::process::Command::new("wsl.exe");
        command.args(plan.wsl_args());
        command
    } else {
        let executable = resolve_host_agent(&plan.agent);
        plan.host_command(executable)
    };
    let status = tokio::process::Command::from(command)
        .status()
        .await
        .context("launch resumed agent with literal session argument")?;
    if !status.success() {
        bail!("resumed agent exited with {status}");
    }
    Ok(())
}

fn resolve_host_agent(agent: &str) -> String {
    let resolved = crate::agent_registry::resolve_bare_agent_name(agent);
    if resolved == agent {
        if let Some(path) = std::env::var_os("PATH") {
            let batch = format!("{agent}.bat");
            if std::env::split_paths(&path).any(|dir| dir.join(&batch).is_file()) {
                return batch;
            }
        }
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDS: &[&str] = &[
        "plain",
        "&echo injected>marker",
        "|echo injected",
        "<in>out",
        "a\"&echo injected>marker&\"",
        "a^b",
        "%WTA_TEST_MARKER%",
        "!WTA_TEST_MARKER!",
        "a`b",
        "$(echo injected)",
        "with spaces",
        "a\\\"&echo injected>marker",
        "\u{4e2d}\u{6587}",
        "a\"b",
        "a\" b",
        "trailing\\",
        "",
        "a'b",
    ];

    fn plan(id: &str) -> ResumeLaunch {
        ResumeLaunch {
            agent: "copilot".into(),
            session_id: id.into(),
            cwd: None,
            distro: None,
        }
    }

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("wta-resume-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let source = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join("fixtures")
                .join("resume_arg_dump.rs");
            let result = std::process::Command::new("rustc")
                .args(["--edition=2021", "--target", "x86_64-pc-windows-msvc"])
                .arg(source)
                .arg("-o")
                .arg(dir.join("dump.exe"))
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            for extension in ["cmd", "bat"] {
                std::fs::write(
                    dir.join(format!("dump.{extension}")),
                    "@echo off\r\n\"%~dp0dump.exe\" %*\r\n",
                )
                .unwrap();
            }
            Self(dir)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn resume_launch_host_native_and_npm_batch_forward_exactly_one_session_argument() {
        let fixture = Fixture::new();
        for extension in ["exe", "cmd", "bat"] {
            for id in IDS {
                let mut command =
                    plan(id).host_command(fixture.0.join(format!("dump.{extension}")));
                let output = command
                    .current_dir(&fixture.0)
                    .env("WTA_TEST_MARKER", "EXPANDED")
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{extension} {id:?}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(
                    String::from_utf8_lossy(&output.stdout).trim(),
                    format!("{:?}", ["--resume", id]),
                    "{extension}"
                );
                assert!(
                    !fixture.0.join("marker").exists(),
                    "no second command may run"
                );
            }
        }
        for extension in ["cmd", "bat"] {
            let error = plan("line1\r\nline2")
                .host_command(fixture.0.join(format!("dump.{extension}")))
                .output()
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn resume_launch_bootstrap_survives_percent_expansion_and_preserves_encoded_data() {
        let fixture = Fixture::new();
        let literal_percent_dir = fixture.0.join("%WTA_TEST_MARKER%");
        std::fs::create_dir(&literal_percent_dir).unwrap();
        let exe = literal_percent_dir.join("dump.exe");
        std::fs::copy(fixture.0.join("dump.exe"), &exe).unwrap();
        let id = IDS.join(" ");
        let plan = plan(&id);
        let encoded = plan.encode().unwrap();
        assert_eq!(ResumeLaunch::decode(&encoded).unwrap(), plan);
        let commandline = bootstrap_commandline(&exe, &encoded).unwrap();
        assert!(
            !commandline.contains('%'),
            "WT expands percent variables before CreateProcess"
        );
        assert!(!commandline.contains(&id));
        let args = crate::coordinator::split_windows_commandline(&commandline);
        let output = std::process::Command::new(&args[0])
            .args(&args[1..])
            .current_dir(&fixture.0)
            .env("WTA_TEST_MARKER", "EXPANDED")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            format!("{:?}", ["resume-session", encoded.as_str()])
        );
        assert!(!fixture.0.join("marker").exists());
    }

    #[test]
    fn resume_launch_wsl_uses_exec_and_login_shell_with_data_only_arguments() {
        for id in IDS {
            let mut plan = plan(id);
            plan.distro = Some("distro & %name%".into());
            plan.cwd = Some("/home/a '\"&%".into());
            let args = plan.wsl_args();
            assert_eq!(
                args,
                [
                    "--distribution",
                    "distro & %name%",
                    "--cd",
                    "/home/a '\"&%",
                    "--exec",
                    "bash",
                    "-lc",
                    "exec \"$@\"",
                    "wta-resume",
                    "copilot",
                    "--resume",
                    id,
                ]
            );
            assert_eq!(ResumeLaunch::decode(&plan.encode().unwrap()).unwrap(), plan);
            assert!(plan
                .banner()
                .contains(&id.chars().take(8).collect::<String>()));
        }
    }

    #[test]
    fn resume_launch_preserves_provider_syntax_and_rejects_only_unrepresentable_nul() {
        for (agent, flag) in [
            ("copilot", "--resume"),
            ("claude", "--resume"),
            ("codex", "resume"),
            ("gemini", "--resume"),
            ("opencode", "--session"),
        ] {
            let mut plan = plan("opaque & id");
            plan.agent = agent.into();
            let command = plan.host_command("fixture.exe");
            assert_eq!(
                command.get_args().collect::<Vec<_>>(),
                [flag, "opaque & id"]
            );
        }
        assert!(plan("bad\0id")
            .encode()
            .unwrap_err()
            .to_string()
            .contains("NUL"));
        assert!(ResumeLaunch::decode("not hex").is_err());
        assert_eq!(
            plan("&echo hi").banner(),
            "\x1b[2;37mResuming copilot session &echo hi...\x1b[0m"
        );
        assert!(
            !plan("a\x1bb").banner().contains("a\x1bb"),
            "control data cannot become a terminal escape"
        );
    }
}
