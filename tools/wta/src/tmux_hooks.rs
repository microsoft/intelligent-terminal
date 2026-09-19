//! Identity and metadata boundary shared by the master and helper hook routers.
//! Remote metadata is display-only; only the controller's native pane GUID can
//! bind or focus a session. A controller-resolved SSH target joins the existing
//! SSH registry; opaque backends retain pane-scoped tmux identities.

use crate::agent_sessions::{pane_key, CliSource, SessionLocation};

#[derive(Debug)]
pub(crate) struct TmuxHook {
    pub location: SessionLocation,
    pub key: Option<String>,
    pub ssh_target: Option<crate::ssh_sessions::SshTarget>,
    pane_id: String,
    cli_source: CliSource,
}

impl TmuxHook {
    /// Sessionless hooks may use only a live binding in this pane and provider.
    /// Remote display metadata can change when a pane moves between tmux
    /// sessions; the controller's stable native GUID remains authoritative.
    pub fn matches_binding(
        &self,
        pane_id: Option<&str>,
        cli_source: Option<&CliSource>,
        location: &SessionLocation,
    ) -> bool {
        matches!(location, SessionLocation::Tmux { .. })
            && cli_source == Some(&self.cli_source)
            && pane_id.map(pane_key).as_deref() == Some(self.pane_id.as_str())
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TmuxMetadata {
    session_id: String,
    pane_id: String,
    session_name: String,
    socket_path: String,
    ssh_target: Option<crate::ssh_sessions::SshTarget>,
}

/// Call only after filtering the raw Copilot sidekick id. `None` is a normal
/// non-tmux hook; any malformed *present* metadata must be dropped, never
/// reinterpreted as a host hook. Errors contain no remote-supplied text.
pub(crate) fn normalize(
    params: &serde_json::Value,
    native_pane_id: &str,
    cli_source: &CliSource,
    raw_session_id: &str,
) -> Result<Option<TmuxHook>, &'static str> {
    let Some(value) = params.get("tmux") else {
        return Ok(None);
    };
    if value
        .get("ssh_target")
        .is_some_and(serde_json::Value::is_null)
    {
        return Err("invalid tmux SSH target");
    }
    let metadata: TmuxMetadata =
        serde_json::from_value(value.clone()).map_err(|_| "invalid tmux metadata shape")?;
    let valid_id = |value: &str, prefix: char| {
        value
            .strip_prefix(prefix)
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
    };
    // The controller may not know these display fields during initial attach.
    let valid_text = |value: &str| value.len() <= 512 && !value.chars().any(char::is_control);
    if !valid_id(&metadata.session_id, '$')
        || !valid_id(&metadata.pane_id, '%')
        || !valid_text(&metadata.session_name)
        || !valid_text(&metadata.socket_path)
        || (!metadata.socket_path.is_empty() && !metadata.socket_path.starts_with('/'))
    {
        return Err("invalid tmux metadata fields");
    }
    if uuid::Uuid::parse_str(native_pane_id).is_err()
        || !params
            .get("pane_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|pane| {
                uuid::Uuid::parse_str(pane).is_ok() && pane_key(pane) == pane_key(native_pane_id)
            })
    {
        return Err("tmux hook missing native pane GUID");
    }
    // Unlike legacy local hooks, absence is allowed but a non-string id is
    // malformed, not an invitation to use another session's binding.
    if params
        .get("agent_session_id")
        .is_some_and(|id| !id.is_string())
        || raw_session_id.chars().any(char::is_control)
    {
        return Err("invalid tmux agent session id");
    }
    let pane_id = pane_key(native_pane_id);
    let cli = match cli_source {
        CliSource::Claude => "claude",
        CliSource::Codex => "codex",
        CliSource::Copilot => "copilot",
        CliSource::Gemini => "gemini",
        CliSource::OpenCode => "opencode",
        CliSource::Unknown(_) => return Err("unknown tmux hook provider"),
    };
    // Byte-length framing is unambiguous even if the raw id contains colons,
    // quotes, Unicode or text resembling another namespaced id.
    let key = (!raw_session_id.is_empty()).then(|| {
        format!(
            "tmux:{}:{}{}:{}{}:{}",
            pane_id.len(),
            pane_id,
            cli.len(),
            cli,
            raw_session_id.len(),
            raw_session_id
        )
    });
    Ok(Some(TmuxHook {
        location: SessionLocation::Tmux {
            session_id: metadata.session_id,
            pane_id: metadata.pane_id,
            session_name: metadata.session_name,
            socket_path: metadata.socket_path,
        },
        key,
        ssh_target: metadata.ssh_target,
        pane_id,
        cli_source: cli_source.clone(),
    }))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const PANE_A: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    pub const PANE_B: &str = "11111111-2222-3333-4444-555555555555";
    pub const PANE_C: &str = "00000000-aaaa-bbbb-cccc-111111111111";

    pub fn hook(event: &str, pane: &str, cli: &str, sid: &str) -> serde_json::Value {
        serde_json::json!({
            "event": event,
            "cli_source": cli,
            "agent_session_id": sid,
            "pane_id": pane,
            "tab_id": 4,
            "window_id": "2",
            "payload": {"cwd": "/home/me/project", "tool_name": "edit", "message": "approve"},
            "tmux": {
                "session_id": "$0",
                "pane_id": "%1",
                "session_name": "work",
                "socket_path": "/tmp/tmux-1000/default"
            }
        })
    }

    pub fn key(pane: &str, cli: &str, sid: &str) -> String {
        normalize(
            &hook("agent.session.start", pane, cli, sid),
            pane,
            &CliSource::parse(Some(cli)),
            sid,
        )
        .unwrap()
        .unwrap()
        .key
        .unwrap()
    }

    pub fn ssh_hook(
        event: &str,
        pane: &str,
        cli: &str,
        sid: &str,
        target: &crate::ssh_sessions::SshTarget,
    ) -> serde_json::Value {
        let mut params = hook(event, pane, cli, sid);
        params["tmux"]["ssh_target"] = serde_json::to_value(target).unwrap();
        params
    }

    #[test]
    fn tmux_ssh_source_requires_valid_controller_metadata() {
        let target = crate::ssh_sessions::SshTarget::new("user@ubuntu", Some(2222)).unwrap();
        let params = ssh_hook("agent.session.start", PANE_A, "copilot", "sid", &target);
        let normalized = normalize(&params, PANE_A, &CliSource::Copilot, "sid")
            .unwrap()
            .unwrap();
        assert_eq!(normalized.ssh_target, Some(target));
        for invalid in [
            serde_json::Value::Null,
            serde_json::json!("ubuntu"),
            serde_json::json!({}),
            serde_json::json!({"destination": "-oProxyCommand=bad"}),
            serde_json::json!({"destination": "ubuntu", "port": 0}),
            serde_json::json!({"destination": "ubuntu", "unexpected": true}),
        ] {
            let mut malformed = params.clone();
            malformed["tmux"]["ssh_target"] = invalid;
            assert!(normalize(&malformed, PANE_A, &CliSource::Copilot, "sid").is_err());
        }
        let mut remote_claim = hook("agent.session.start", PANE_A, "copilot", "sid");
        remote_claim["payload"]["ssh_target"] = params["tmux"]["ssh_target"].clone();
        assert!(normalize(&remote_claim, PANE_A, &CliSource::Copilot, "sid")
            .unwrap()
            .unwrap()
            .ssh_target
            .is_none());
    }

    #[test]
    fn tmux_identity_scopes_native_pane_provider_and_raw_id() {
        let a = key(PANE_A, "copilot", "same-id");
        assert_ne!(a, "same-id");
        assert_ne!(a, key(PANE_B, "copilot", "same-id"));
        assert_ne!(a, key(PANE_A, "claude", "same-id"));
        assert_ne!(a, key(PANE_A, "copilot", "different-id"));
        assert_eq!(
            a,
            key(
                &format!("{{{}}}", PANE_A.to_uppercase()),
                "Copilot",
                "same-id"
            )
        );
        let framed = key(PANE_A, "copilot", "0:claude7:ü");
        assert_ne!(framed, key(PANE_A, "claude", "7:ü"));
        assert_ne!(framed, key(PANE_A, "copilot", &framed));
    }

    #[test]
    fn tmux_metadata_must_be_complete_typed_and_safe_for_display() {
        let good = hook("agent.session.start", PANE_A, "copilot", "sid");
        for metadata in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!({"session_id": "$0", "pane_id": "%1", "session_name": "work"}),
            serde_json::json!({"session_id": "$0", "pane_id": "%1", "session_name": "work", "socket_path": "/socket", "unexpected": true}),
        ] {
            let mut params = good.clone();
            params["tmux"] = metadata;
            assert!(normalize(&params, PANE_A, &CliSource::Copilot, "sid").is_err());
        }
        for (field, value) in [
            ("session_id", "work"),
            ("pane_id", "native-guid"),
            ("session_name", "\x1b[31mspoof"),
            ("socket_path", "relative/path"),
        ] {
            let mut params = good.clone();
            params["tmux"][field] = value.into();
            assert!(normalize(&params, PANE_A, &CliSource::Copilot, "sid").is_err());
        }
        assert!(normalize(&good, "", &CliSource::Copilot, "sid").is_err());
        assert!(normalize(&good, "%1", &CliSource::Copilot, "sid").is_err());
        let mut no_native_pane = good.clone();
        no_native_pane.as_object_mut().unwrap().remove("pane_id");
        assert!(normalize(&no_native_pane, PANE_A, &CliSource::Copilot, "sid").is_err());
        assert!(normalize(&good, PANE_B, &CliSource::Copilot, "sid").is_err());
        let mut invalid_id = good.clone();
        invalid_id["agent_session_id"] = serde_json::Value::Null;
        assert!(normalize(&invalid_id, PANE_A, &CliSource::Copilot, "").is_err());
        let mut local = good;
        local.as_object_mut().unwrap().remove("tmux");
        assert!(normalize(&local, "legacy-pane", &CliSource::Copilot, "sid")
            .unwrap()
            .is_none());
    }

    #[test]
    fn tmux_initial_attach_allows_empty_display_fields_with_utf8_byte_limits() {
        let mut params = hook("agent.session.start", PANE_A, "copilot", "sid");
        params["tmux"]["session_name"] = "".into();
        params["tmux"]["socket_path"] = "".into();
        assert!(normalize(&params, PANE_A, &CliSource::Copilot, "sid").is_ok());
        for (field, value) in [
            ("session_name", "é".repeat(256)),
            ("socket_path", format!("/{}", "a".repeat(511))),
        ] {
            params["tmux"][field] = value.clone().into();
            assert!(normalize(&params, PANE_A, &CliSource::Copilot, "sid").is_ok());
            params["tmux"][field] = format!("{value}é").into();
            assert!(normalize(&params, PANE_A, &CliSource::Copilot, "sid").is_err());
            params["tmux"][field] = "".into();
        }
    }
}
