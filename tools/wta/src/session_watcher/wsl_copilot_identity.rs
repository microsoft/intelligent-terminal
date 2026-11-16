//! Exact native WSL Copilot identity, without installing hooks or reading transcripts.

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::io::AsyncReadExt;

pub(crate) const MAX_SESSIONS: usize = 128;
const MAX_OUTPUT: u64 = 32 * 1024;
const PROBE: &str = r#"
set -eu
root="${COPILOT_HOME:-$HOME/.copilot}/session-state"
[ -d "$root" ] || exit 0
root=$(cd "$root" && pwd -P)
for session do
    directory="$root/$session"
    [ -d "$directory" ] || continue
    count=0
    for marker in "$directory"/inuse.*.lock; do
        [ -f "$marker" ] || continue
        count=$((count + 1))
        [ "$count" -le 64 ] || exit 2
        pid="${marker##*/inuse.}"
        pid="${pid%.lock}"
        case "$pid" in ''|*[!0-9]*) continue;; esac
        [ "$(head -c 64 "$marker")" = "$pid" ] || continue
        executable=$(readlink "/proc/$pid/exe") || continue
        [ "${executable##*/}" = copilot ] || continue
        held=false
        descriptors=0
        for descriptor in "/proc/$pid"/fd/*; do
            descriptors=$((descriptors + 1))
            [ "$descriptors" -le 512 ] || break
            target=$(readlink "$descriptor") || continue
            if [ "$target" = "$directory/inuse.$pid.hold" ]; then
                held=true
                break
            fi
        done
        [ "$held" = true ] || continue
        [ -r "/proc/$pid/environ" ] || continue
        pane=$(tr '\000' '\n' < "/proc/$pid/environ" | sed -n 's/^WT_SESSION=//p')
        package=$(tr '\000' '\n' < "/proc/$pid/environ" | sed -n 's/^WT_COM_CLSID=//p')
        [ -n "$pane" ] && [ -n "$package" ] || continue
        printf '%s\t%s\t%s\n' "$session" "$pane" "$package"
    done
done
"#;

fn parse_leases(
    bytes: &[u8],
    sessions: &[String],
    package: uuid::Uuid,
) -> Result<Vec<(uuid::Uuid, uuid::Uuid)>> {
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_OUTPUT,
        "WSL lease output is too large"
    );
    let text = std::str::from_utf8(bytes).context("decode WSL lease identities")?;
    let requested: HashSet<_> = sessions
        .iter()
        .filter_map(|id| uuid::Uuid::parse_str(id).ok())
        .collect();
    let mut leases = Vec::new();
    let mut session_claims = HashMap::new();
    let mut pane_claims = HashMap::new();
    for line in text.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        let [session, pane, owner] = fields.as_slice() else {
            continue;
        };
        if uuid::Uuid::parse_str(owner).ok() != Some(package) {
            continue;
        }
        let (Ok(session), Ok(pane)) = (uuid::Uuid::parse_str(session), uuid::Uuid::parse_str(pane))
        else {
            continue;
        };
        if !requested.contains(&session) || pane.is_nil() {
            continue;
        }
        *session_claims.entry(session).or_insert(0) += 1;
        *pane_claims.entry(pane).or_insert(0) += 1;
        leases.push((session, pane));
    }
    leases.retain(|(session, pane)| {
        session_claims.get(session) == Some(&1) && pane_claims.get(pane) == Some(&1)
    });
    Ok(leases)
}

pub(crate) async fn probe(
    distro: &str,
    sessions: &[String],
) -> Result<Vec<(uuid::Uuid, uuid::Uuid)>> {
    anyhow::ensure!(
        !sessions.is_empty()
            && sessions.len() <= MAX_SESSIONS
            && sessions
                .iter()
                .all(|id| uuid::Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == *id)),
        "Invalid WSL lease session inventory"
    );
    let package = std::env::var("WT_COM_CLSID")
        .context("native lease probe requires the owning Terminal package")?;
    let package =
        uuid::Uuid::parse_str(&package).context("invalid owning Terminal package UUID")?;
    crate::agent_source::ensure_wsl_running(distro).await?;
    let mut command = tokio::process::Command::new("wsl.exe");
    command
        .args([
            "--distribution",
            distro,
            "--exec",
            "timeout",
            "2s",
            "sh",
            "-c",
            PROBE,
            "sh",
        ])
        .args(sessions)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let mut child = command.spawn().context("start native WSL lease probe")?;
    let stdout = child
        .stdout
        .take()
        .context("WSL lease probe stdout missing")?;
    let output = tokio::time::timeout(Duration::from_secs(3), async {
        let mut bytes = Vec::new();
        stdout.take(MAX_OUTPUT + 1).read_to_end(&mut bytes).await?;
        anyhow::ensure!(
            bytes.len() as u64 <= MAX_OUTPUT,
            "WSL lease output is too large"
        );
        anyhow::ensure!(child.wait().await?.success(), "WSL lease probe failed");
        Ok::<_, anyhow::Error>(bytes)
    })
    .await
    .context("native WSL lease probe timed out")??;
    parse_leases(&output, sessions, package)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_identity_is_exact_package_qualified_and_unambiguous() {
        let session = uuid::Uuid::new_v4();
        let other = uuid::Uuid::new_v4();
        let pane = uuid::Uuid::new_v4();
        let package = uuid::Uuid::new_v4();
        let sessions = vec![session.to_string(), other.to_string()];
        let row = format!("{session}\t{pane}\t{{{package}}}\n");
        assert_eq!(
            parse_leases(row.as_bytes(), &sessions, package).unwrap(),
            vec![(session, pane)]
        );
        assert!(
            parse_leases(row.as_bytes(), &sessions, uuid::Uuid::new_v4())
                .unwrap()
                .is_empty()
        );
        assert!(
            parse_leases(format!("{row}{row}").as_bytes(), &sessions, package)
                .unwrap()
                .is_empty()
        );
        assert!(parse_leases(
            format!("{row}{other}\t{pane}\t{package}\n").as_bytes(),
            &sessions,
            package
        )
        .unwrap()
        .is_empty());
        assert!(parse_leases(
            format!("{row}{session}\t{other}\t{package}\n").as_bytes(),
            &sessions,
            package
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn malformed_unrequested_or_unbounded_evidence_cannot_bind() {
        let session = uuid::Uuid::new_v4();
        let pane = uuid::Uuid::new_v4();
        let package = uuid::Uuid::new_v4();
        for output in [
            "invalid".to_string(),
            format!("{session}\t{}\t{package}\n", "not-a-pane"),
            format!("{session}\t{}\t{package}\n", uuid::Uuid::nil()),
            format!("{}\t{pane}\t{package}\n", uuid::Uuid::new_v4()),
            format!("{session}\t{pane}\t{package}\t{}\n", "extra"),
            "x".repeat(MAX_OUTPUT as usize + 1),
        ] {
            assert!(
                parse_leases(output.as_bytes(), &[session.to_string()], package)
                    .map_or(true, |leases| leases.is_empty())
            );
        }
    }

    #[test]
    fn unrelated_missing_or_malformed_terminal_attribution_does_not_hide_valid_leases() {
        let session = uuid::Uuid::new_v4();
        let pane = uuid::Uuid::new_v4();
        let package = uuid::Uuid::new_v4();
        let output = format!(
            "{session}\t\t\n{session}\t{}\t{package}\n\
            {session}\t{pane}\t{}\n{session}\t{pane}\t{package}\n",
            "invalid", "other-package"
        );
        assert_eq!(
            parse_leases(output.as_bytes(), &[session.to_string()], package).unwrap(),
            vec![(session, pane)]
        );
    }
}
