//! User-owned ACP, interactive and resume invocations, resolved from Terminal settings.

use anyhow::{bail, ensure, Context, Result};
use serde::Deserialize;

#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Invocation {
    pub(crate) command: String,
    #[serde(default)]
    pub(crate) args: Vec<String>,
    #[serde(default)]
    pub(crate) can_resume_acp_sessions: bool,
}

#[derive(Clone, Deserialize)]
pub(crate) struct CustomAgent {
    pub(crate) id: String,
    #[serde(default, rename = "name")]
    _name: String,
    pub(crate) acp: Option<Invocation>,
    pub(crate) cli: Option<Invocation>,
    pub(crate) resume: Option<Invocation>,
}

pub(crate) fn is_custom_id(id: &str) -> bool {
    id.strip_prefix("custom:").is_some_and(|name| {
        !name.is_empty()
            && name
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_' | b'.'))
    })
}

pub(crate) fn activation_identity(
    session: &crate::agent_sessions::AgentSession,
    snapshot: Option<&[crate::session_registry::SessionInfo]>,
) -> Result<crate::session_registry::SessionIdentity> {
    use crate::session_registry::{agent_session_to_session_info, SessionIdentity};
    let identity = SessionIdentity::from_info(&agent_session_to_session_info(session));
    let Some(snapshot) = snapshot else {
        return Ok(identity);
    };
    let mut candidates = snapshot.iter().filter(|row| {
        let mut candidate = SessionIdentity::from_info(row);
        if let Some(key) = candidate.history_key.as_mut() {
            key.session_universe = None;
        }
        candidate == identity
    });
    let row = candidates
        .next()
        .context("The selected custom session is no longer available.")?;
    ensure!(
        candidates.next().is_none(),
        "The selected custom session is ambiguous across session stores."
    );
    Ok(SessionIdentity::from_info(row))
}

impl Invocation {
    fn validate(&self, resume: bool) -> Result<()> {
        ensure!(
            !self.command.trim().is_empty()
                && !self.command.contains(['\0', '\r', '\n'])
                && !self.command.contains("{sessionId}"),
            "invalid custom agent executable"
        );
        ensure!(
            self.args.iter().all(|arg| !arg.contains('\0')),
            "custom agent arguments cannot contain NUL"
        );
        if resume {
            ensure!(
                self.args.iter().filter(|arg| *arg == "{sessionId}").count() == 1
                    && self
                        .args
                        .iter()
                        .all(|arg| arg == "{sessionId}" || !arg.contains(['{', '}'])),
                "custom resume requires exactly one complete {{sessionId}} argument"
            );
        }
        Ok(())
    }

    pub(crate) fn commandline(&self) -> String {
        std::iter::once(&self.command)
            .chain(&self.args)
            .map(|arg| crate::coordinator::quote_windows_commandline_arg(arg))
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub(crate) fn resume_commandline(&self, session_id: &str) -> Result<String> {
        self.validate(true)?;
        ensure!(
            crate::agent_sessions::is_safe_cli_resume_id(session_id)
                && !session_id.starts_with('-'),
            "invalid custom CLI resume identifier"
        );
        let args: Vec<_> = self
            .args
            .iter()
            .map(|arg| {
                if arg == "{sessionId}" {
                    session_id
                } else {
                    arg.as_str()
                }
            })
            .collect();
        ensure!(
            !self.command.contains('%') && args.iter().all(|arg| !arg.contains('%')),
            "environment references are unsupported in custom CLI resume commands"
        );
        let extension = std::path::Path::new(&self.command)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("");
        if extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat") {
            // Never interpolate an identifier through cmd's parser. Use the
            // exact npm shim's PowerShell companion, not a different PATH entry.
            let configured = std::path::PathBuf::from(&self.command);
            let shim = if configured.is_file() {
                Some(configured)
            } else if !self.command.contains(['\\', '/']) {
                std::env::var_os("PATH").and_then(|path| {
                    std::env::split_paths(&path)
                        .map(|directory| directory.join(&self.command))
                        .find(|candidate| candidate.is_file())
                })
            } else {
                None
            }
            .context("custom resume batch launcher is unavailable")?;
            let shim = if shim.is_absolute() {
                shim
            } else {
                std::env::current_dir()
                    .context("resolve custom batch launcher")?
                    .join(shim)
            };
            let companion = shim.with_extension("ps1");
            ensure!(
                companion.is_file(),
                "custom batch resume requires an adjacent PowerShell companion"
            );
            let tokens = std::iter::once(companion.to_string_lossy().into_owned())
                .chain(args.iter().map(|arg| (*arg).to_string()))
                .map(|arg| crate::coordinator::ps_single_quote(&arg))
                .collect::<Vec<_>>()
                .join(" ");
            let script = format!("$ErrorActionPreference='Stop'; & {tokens}; exit $LASTEXITCODE");
            let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
            return Ok(format!(
                "powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -EncodedCommand {}",
                crate::osc52::base64_encode(&utf16)
            ));
        }
        Ok(std::iter::once(self.command.as_str())
            .chain(args)
            .map(crate::coordinator::quote_windows_commandline_arg)
            .collect::<Vec<_>>()
            .join(" "))
    }
}

impl CustomAgent {
    fn validate(&self) -> Result<()> {
        ensure!(is_custom_id(&self.id), "invalid custom agent ID");
        ensure!(
            self.acp.is_some() || self.cli.is_some(),
            "custom agent requires ACP or interactive CLI startup"
        );
        for invocation in [&self.acp, &self.cli].into_iter().flatten() {
            invocation.validate(false)?;
        }
        if let Some(resume) = &self.resume {
            ensure!(
                self.cli.is_some(),
                "custom resume requires an interactive CLI command"
            );
            resume.validate(true)?;
        }
        Ok(())
    }
}

fn definitions(settings: &serde_json::Value) -> Result<Vec<CustomAgent>> {
    let Some(value) = settings.get("customAgents") else {
        return Ok(Vec::new());
    };
    let agents: Vec<CustomAgent> =
        serde_json::from_value(value.clone()).context("invalid customAgents configuration")?;
    let mut ids = std::collections::HashSet::new();
    for agent in &agents {
        agent.validate()?;
        if !ids.insert(agent.id.to_ascii_lowercase()) {
            bail!("duplicate custom agent ID");
        }
    }
    Ok(agents)
}

pub(crate) fn from_settings(settings: &serde_json::Value, id: &str) -> Result<Option<CustomAgent>> {
    Ok(definitions(settings)?
        .into_iter()
        .find(|agent| agent.id.eq_ignore_ascii_case(id)))
}

pub(crate) fn history_commands(settings: &serde_json::Value) -> Result<Vec<(String, String)>> {
    let agents = definitions(settings)?;
    let mut ids: std::collections::HashSet<_> = agents
        .iter()
        .map(|agent| agent.id.to_ascii_lowercase())
        .collect();
    let mut commands: Vec<_> = agents
        .into_iter()
        .filter_map(|agent| agent.acp.map(|acp| (agent.id, acp.commandline())))
        .collect();
    let legacy: Vec<String> = match settings.get("acpCustomCommands") {
        Some(value) => {
            serde_json::from_value(value.clone()).context("invalid legacy custom ACP commands")?
        }
        None => Vec::new(),
    };
    // The singular setting mirrors the selection, including structured entries.
    // Do not turn that compatibility mirror into another executable-derived ID.
    let selected_legacy = settings
        .get("acpAgent")
        .and_then(serde_json::Value::as_str)
        .filter(|id| {
            id.get(..7)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("custom:"))
                && !ids.contains(&id.to_ascii_lowercase())
        })
        .and_then(|id| {
            settings
                .get("acpCustomCommand")
                .and_then(serde_json::Value::as_str)
                .map(|command| (Some(id), command.to_owned()))
        });
    for (selected_id, command) in selected_legacy
        .into_iter()
        .chain(legacy.into_iter().map(|command| (None, command)))
    {
        if command.trim().is_empty() {
            continue;
        }
        ensure!(
            !command.contains(['\0', '\r', '\n']),
            "invalid legacy custom ACP command"
        );
        let tokens = crate::coordinator::split_windows_commandline(&command);
        let executable = tokens.first().context("missing custom ACP executable")?;
        let name = executable.rsplit(['\\', '/']).next().unwrap_or(executable);
        let name = match name.rsplit_once('.') {
            Some((stem, extension))
                if !stem.is_empty()
                    && ["exe", "cmd", "bat"]
                        .iter()
                        .any(|candidate| extension.eq_ignore_ascii_case(candidate)) =>
            {
                stem
            }
            _ => name,
        };
        ensure!(!name.is_empty(), "missing custom ACP executable name");
        let id = format!("custom:{name}");
        if selected_id.is_some_and(|selected| !selected.eq_ignore_ascii_case(&id)) {
            continue;
        }
        if ids.insert(id.to_ascii_lowercase()) {
            commands.push((id, command));
        }
    }
    Ok(commands)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn history_catalog_includes_unselected_acp_agents_and_excludes_cli_only() {
        let settings = json!({
            "acpAgent": "copilot",
            "customAgents": [
                {"id": "custom:one", "acp": {"command": "adapter.exe", "args": ["--profile", "one"]}},
                {"id": "custom:two", "acp": {"command": "adapter.exe", "args": ["--profile", "two"]}},
                {"id": "custom:cli-only", "cli": {"command": "interactive.exe"}}
            ]
        });
        assert_eq!(
            history_commands(&settings).unwrap(),
            [
                ("custom:one".into(), "adapter.exe --profile one".into()),
                ("custom:two".into(), "adapter.exe --profile two".into()),
            ]
        );
    }

    #[test]
    fn history_catalog_preserves_legacy_identity_without_duplicating_structured_entries() {
        let settings = json!({
            "acpAgent": "custom:stable",
            "acpCustomCommand": "node.exe cli-entry.js --acp",
            "acpCustomCommands": [
                "\"C:\\agent dir\\Legacy.CMD\" --stdio",
                "Legacy.CMD --different",
                "cli-only.exe --stdio",
                "\u{52a9}\u{624b} --stdio",
                "\u{53e6}\u{4e00}\u{4e2a}.EXE --stdio"
            ],
            "customAgents": [
                {"id": "custom:stable", "acp": {"command": "node.exe", "args": ["cli-entry.js", "--acp"]}},
                {"id": "custom:cli-only", "cli": {"command": "cli-only.exe"}}
            ]
        });
        assert_eq!(
            history_commands(&settings).unwrap(),
            [
                ("custom:stable".into(), "node.exe cli-entry.js --acp".into()),
                (
                    "custom:Legacy".into(),
                    "\"C:\\agent dir\\Legacy.CMD\" --stdio".into()
                ),
                (
                    "custom:\u{52a9}\u{624b}".into(),
                    "\u{52a9}\u{624b} --stdio".into()
                ),
                (
                    "custom:\u{53e6}\u{4e00}\u{4e2a}".into(),
                    "\u{53e6}\u{4e00}\u{4e2a}.EXE --stdio".into()
                ),
            ]
        );
        assert_eq!(
            history_commands(&json!({
                "acpAgent": "custom:legacy",
                "acpCustomCommand": "legacy.BAT --stdio"
            }))
            .unwrap(),
            [("custom:legacy".into(), "legacy.BAT --stdio".into())]
        );
        assert!(history_commands(&json!({
            "acpAgent": "copilot",
            "acpCustomCommand": "inactive-legacy.exe --stdio"
        }))
        .unwrap()
        .is_empty());
    }

    #[test]
    fn history_catalog_prefers_only_the_matching_selected_legacy_command() {
        for (selection, expected) in [
            ("CUSTOM:WORK", "work.exe --new"),
            ("custom:other", "work.exe --old"),
            ("copilot", "work.exe --old"),
        ] {
            let settings = json!({
                "acpAgent": selection,
                "acpCustomCommand": "work.exe --new",
                "acpCustomCommands": ["work.exe --old"]
            });
            assert_eq!(
                history_commands(&settings).unwrap(),
                [("custom:work".into(), expected.into())]
            );
        }
        let settings = json!({
            "acpAgent": "custom:work",
            "acpCustomCommand": "work.exe --new",
            "acpCustomCommands": ["work.exe --old"],
            "customAgents": [
                {"id": "custom:work", "acp": {"command": "structured.exe"}}
            ]
        });
        assert_eq!(
            history_commands(&settings).unwrap(),
            [("custom:work".into(), "structured.exe".into())]
        );
    }

    #[test]
    fn history_catalog_rejects_invalid_settings() {
        for settings in [
            json!({"customAgents": [
                {"id": "custom:one", "acp": {"command": "one.exe"}},
                {"id": "custom:ONE", "acp": {"command": "two.exe"}}
            ]}),
            json!({"customAgents": [{"id": "custom:broken", "acp": {"command": ""}}]}),
            json!({"acpCustomCommands": ["agent.exe\n--stdio"]}),
            json!({"acpCustomCommands": "agent.exe --stdio"}),
        ] {
            assert!(history_commands(&settings).is_err());
        }
    }

    #[test]
    fn custom_invocations_are_independent_and_identity_is_explicit() {
        let settings = json!({"customAgents": [{
            "id": "custom:work", "name": "Work",
            "acp": {"command": "npx.exe", "args": ["adapter", "--stdio"]},
            "cli": {"command": "work.exe", "args": ["--config", "C:\\config dir\\work.json"]},
            "resume": {"command": "work.exe", "args": ["resume", "{sessionId}"]}
        }]});
        let agent = from_settings(&settings, "custom:work").unwrap().unwrap();
        assert_eq!(agent.acp.unwrap().commandline(), "npx.exe adapter --stdio");
        assert_eq!(
            agent.cli.unwrap().commandline(),
            r#"work.exe --config "C:\config dir\work.json""#
        );
        let resume = agent.resume.unwrap();
        assert!(!resume.can_resume_acp_sessions);
        assert_eq!(
            resume.resume_commandline("abc-123").unwrap(),
            "work.exe resume abc-123"
        );
    }

    #[test]
    fn resume_validates_placeholders_and_untrusted_session_ids() {
        for args in [
            vec!["resume"],
            vec!["--id={sessionId}"],
            vec!["{sessionId}", "{sessionId}"],
            vec!["{cwd}", "{sessionId}"],
        ] {
            let invocation = Invocation {
                command: "agent.exe".into(),
                args: args.into_iter().map(str::to_string).collect(),
                can_resume_acp_sessions: false,
            };
            assert!(invocation.resume_commandline("abc").is_err());
        }
        let invocation = Invocation {
            command: "agent.exe".into(),
            args: vec!["--session".into(), "{sessionId}".into()],
            can_resume_acp_sessions: false,
        };
        for id in ["", "abc & calc", "-option", "sidekick-1", "a\nb"] {
            assert!(invocation.resume_commandline(id).is_err(), "{id:?}");
        }
    }

    #[test]
    fn duplicate_ids_and_resume_without_cli_fail_explicitly() {
        let duplicate = json!({"customAgents": [
            {"id": "custom:work", "cli": {"command": "agent.exe"}},
            {"id": "custom:WORK", "cli": {"command": "other.exe"}}
        ]});
        assert!(from_settings(&duplicate, "custom:work").is_err());
        let invalid = json!({"customAgents": [{
            "id": "custom:work", "acp": {"command": "adapter.exe"},
            "resume": {"command": "agent.exe", "args": ["{sessionId}"]}
        }]});
        assert!(from_settings(&invalid, "custom:work").is_err());
        assert!(from_settings(&json!({}), "custom:work").unwrap().is_none());
    }

    #[test]
    fn custom_identity_roundtrips_and_filters_separately_from_builtin_sources() {
        use crate::agent_sessions::CliSource;
        use crate::session_registry::SessionHookCliSource;
        let custom = CliSource::from_agent_id("custom:work").unwrap();
        assert_eq!(CliSource::parse(Some("custom:work")), custom);
        assert_eq!(
            custom.canonical_provider_id().as_deref(),
            Some("custom:work")
        );
        let wire = SessionHookCliSource::from(&custom);
        let decoded: SessionHookCliSource =
            serde_json::from_value(serde_json::to_value(wire).unwrap()).unwrap();
        assert_eq!(CliSource::from(decoded), custom);
        let legacy = SessionHookCliSource::Unknown {
            value: "custom:work".into(),
        };
        assert_eq!(CliSource::from(legacy), custom);
        assert_ne!(
            CliSource::from_agent_id("custom:other"),
            Some(custom.clone())
        );
        assert_ne!(custom, CliSource::Copilot);
    }

    #[test]
    fn activation_preserves_session_store_identity_and_rejects_ambiguity() {
        use crate::agent_sessions::{CliSource, SessionLocation};
        use crate::session_registry::{SessionIdentity, SessionInfo};
        let mut row = SessionInfo::new("shared-id".to_string().into(), "C:\\work".into());
        row.cli_source = Some(CliSource::Custom("custom:work".into()));
        row.provider_id = Some("custom:work".into());
        row.location = SessionLocation::Host;
        row.session_universe = Some("store-one".into());
        let session = crate::app::session_info_to_agent_session(&row);
        assert_eq!(
            activation_identity(&session, Some(&[row.clone()])).unwrap(),
            SessionIdentity::from_info(&row)
        );
        let mut other = row.clone();
        other.cli_source = Some(CliSource::Custom("custom:other".into()));
        other.provider_id = Some("custom:other".into());
        assert_eq!(
            activation_identity(&session, Some(&[row.clone(), other])).unwrap(),
            SessionIdentity::from_info(&row)
        );
        other = row.clone();
        other.session_universe = Some("store-two".into());
        assert!(activation_identity(&session, Some(&[row, other])).is_err());
        assert!(activation_identity(&session, Some(&[])).is_err());
    }

    #[test]
    fn batch_resume_uses_exact_companion_and_preserves_literal_arguments() {
        use std::os::windows::process::CommandExt;

        const ISOLATED_TEST: &str = "WTA_TEST_RESTRICTED_RESUME_ARGV";
        let test_name = concat!(
            module_path!(),
            "::batch_resume_uses_exact_companion_and_preserves_literal_arguments"
        )
        .split_once("::")
        .unwrap()
        .1;
        if std::env::var(ISOLATED_TEST).as_deref() != Ok(test_name) {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
                .env(ISOLATED_TEST, test_name)
                .env("PSExecutionPolicyPreference", "Restricted")
                // Probe Windows PowerShell's policy module, not an inherited PS7 module.
                .env_remove("PSModulePath")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("1 passed;"),
                "the isolated subprocess must run the selected test: {output:?}"
            );
            return;
        }

        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("Generated Files")
            .join(format!(
                "custom O\u{2019}Brien \u{2018}\u{201a}\u{201b} {{agent}} {}",
                uuid::Uuid::new_v4()
            ));
        std::fs::create_dir_all(&directory).unwrap();
        let shim = directory.join("agent.cmd");
        std::fs::write(&shim, "@exit /b 88\r\n").unwrap();
        let invocation = Invocation {
            command: shim.to_string_lossy().into_owned(),
            args: vec![
                "resume".into(),
                "{sessionId}".into(),
                "quote \" and ' & | ! ^ don\u{2019}t \u{2018}\u{201a}\u{201b}".into(),
            ],
            can_resume_acp_sessions: false,
        };
        assert!(invocation.resume_commandline("abc-123").is_err());
        std::fs::write(directory.join("agent.ps1"),
            "$ErrorActionPreference='Stop'\r\n[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'args.json'), (ConvertTo-Json -Compress -InputObject @($args)))\r\nexit 0\r\n"
        ).unwrap();
        let commandline = invocation.resume_commandline("abc-123").unwrap();
        let tokens = crate::coordinator::split_windows_commandline(&commandline);
        let policy = std::process::Command::new(&tokens[0])
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Get-ExecutionPolicy; Get-ExecutionPolicy -Scope Process",
            ])
            .output()
            .unwrap();
        assert!(policy.status.success(), "{policy:?}");
        assert_eq!(
            String::from_utf8(policy.stdout)
                .unwrap()
                .lines()
                .map(str::trim)
                .collect::<Vec<_>>(),
            ["Restricted", "Restricted"]
        );
        let old_launcher = std::process::Command::new(&tokens[0])
            .args(
                tokens[1..]
                    .iter()
                    .filter(|arg| arg.as_str() != "-ExecutionPolicy" && arg.as_str() != "Bypass"),
            )
            .output()
            .unwrap();
        assert!(!old_launcher.status.success(), "{old_launcher:?}");
        assert!(
            String::from_utf8_lossy(&old_launcher.stderr).contains("UnauthorizedAccess"),
            "{old_launcher:?}"
        );
        assert!(!directory.join("args.json").exists());
        let status = std::process::Command::new(&tokens[0])
            .args(&tokens[1..])
            .status()
            .unwrap();
        assert!(status.success(), "{status}");
        let args: Vec<String> =
            serde_json::from_slice(&std::fs::read(directory.join("args.json")).unwrap()).unwrap();
        assert_eq!(
            args,
            vec![
                "resume",
                "abc-123",
                "quote \" and ' & | ! ^ don\u{2019}t \u{2018}\u{201a}\u{201b}"
            ]
        );

        let source = directory.join("argv.cs");
        let executable = directory.join("native argv.exe");
        std::fs::write(
            &source,
            r#"using System; using System.Text;
class Probe { static void Main(string[] args) {
foreach (string arg in args) Console.WriteLine(Convert.ToBase64String(Encoding.UTF8.GetBytes(arg)));
} }"#,
        )
        .unwrap();
        let compiler = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join(r"Microsoft.NET\Framework64\v4.0.30319\csc.exe");
        let compiled = std::process::Command::new(compiler)
            .arg("/nologo")
            .arg(format!("/out:{}", executable.display()))
            .arg(&source)
            .output()
            .unwrap();
        assert!(compiled.status.success(), "{compiled:?}");
        std::fs::write(
            shim.with_extension("ps1"),
            "& \"$PSScriptRoot\\native argv.exe\" @args\r\nexit $LASTEXITCODE\r\n",
        )
        .unwrap();
        let bat = shim.with_extension("bat");
        std::fs::write(&bat, "@exit /b 88\r\n").unwrap();
        for program in [&shim, &bat, &executable] {
            let mut invocation = Invocation {
                command: program.to_string_lossy().into_owned(),
                args: vec![
                    "resume".into(),
                    "{sessionId}".into(),
                    "literal ' & | ! ^".into(),
                    "don\u{2019}t \u{2018}\u{201a}\u{201b} $env:PATH $(throw 'not literal')".into(),
                ],
                can_resume_acp_sessions: false,
            };
            if program == &executable {
                invocation.args.extend([
                    String::new(),
                    "embedded \"quotes\"".into(),
                    r"C:\space here\".into(),
                ]);
            }
            let commandline = invocation.resume_commandline("abc-123").unwrap();
            let tokens = crate::coordinator::split_windows_commandline(&commandline);
            let mut command = std::process::Command::new(&tokens[0]);
            if program == &executable {
                let executable_token =
                    crate::coordinator::quote_windows_commandline_arg(&invocation.command);
                command.raw_arg(
                    commandline
                        .strip_prefix(&executable_token)
                        .unwrap()
                        .trim_start(),
                );
            } else {
                command.args(&tokens[1..]);
            }
            let output = command.output().unwrap();
            assert!(output.status.success(), "{program:?}: {output:?}");
            let expected: Vec<_> = invocation
                .args
                .iter()
                .map(|arg| {
                    crate::osc52::base64_encode(
                        if arg == "{sessionId}" { "abc-123" } else { arg }.as_bytes(),
                    )
                })
                .collect();
            assert_eq!(
                String::from_utf8(output.stdout)
                    .unwrap()
                    .lines()
                    .collect::<Vec<_>>(),
                expected,
                "{program:?}"
            );
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
}
