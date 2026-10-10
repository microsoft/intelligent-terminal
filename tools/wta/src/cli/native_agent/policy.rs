//! Fresh, fail-closed policy reads for a launcher that has no C++ host policy snapshot.

use anyhow::{bail, Context, Result};

#[derive(Debug, Default)]
pub(super) struct Policy {
    pub(super) allowed_agents: Option<Vec<String>>,
    pub(super) automatic_approval_blocked: bool,
}

impl Policy {
    pub(super) fn agent_allowed(&self, id: &str) -> bool {
        self.allowed_agents
            .as_ref()
            .is_none_or(|allowed| allowed.iter().any(|value| value.eq_ignore_ascii_case(id)))
    }

    pub(super) fn check(&self, id: &str) -> Result<()> {
        if !self.agent_allowed(id) {
            bail!("AllowedAgents: {id}");
        }
        // Startup flags are not an enforcement boundary: native agents also
        // read their own config/environment and permit in-session mode changes.
        // Do not promise a policy-enforced interactive session we cannot supply.
        if self.automatic_approval_blocked {
            bail!(
                "{}",
                t!(
                    "system.provider_command_blocked_by_policy",
                    command = "launch-agent"
                )
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hive {
    Machine,
    User,
}

fn resolve(
    mut read_value: impl FnMut(Hive, &str, u32) -> Result<Option<Vec<u8>>>,
) -> Result<Policy> {
    use windows_sys::Win32::System::Registry::{RRF_RT_REG_DWORD, RRF_RT_REG_MULTI_SZ};

    let mut read = |name, kind| -> Result<Option<Vec<u8>>> {
        match read_value(Hive::Machine, name, kind)? {
            Some(value) => Ok(Some(value)),
            None => read_value(Hive::User, name, kind),
        }
    };
    let allowed_agents = read("AllowedAgents", RRF_RT_REG_MULTI_SZ)?
        .map(|bytes| parse_allowlist(&bytes))
        .transpose()?;
    let automatic_approval_blocked = match read("AllowAutomaticApproval", RRF_RT_REG_DWORD)? {
        Some(bytes) => {
            let value: [u8; 4] = bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid AllowAutomaticApproval DWORD"))?;
            u32::from_le_bytes(value) == 0
        }
        None => false,
    };
    Ok(Policy {
        allowed_agents,
        automatic_approval_blocked,
    })
}

fn parse_allowlist(bytes: &[u8]) -> Result<Vec<String>> {
    if bytes.len() % 2 != 0 || bytes.len() < 4 {
        bail!("invalid AllowedAgents REG_MULTI_SZ");
    }
    let units: Vec<_> = bytes
        .chunks_exact(2)
        .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
        .collect();
    if !units.ends_with(&[0, 0]) {
        bail!("unterminated AllowedAgents REG_MULTI_SZ");
    }
    let mut allowed = Vec::new();
    for value in units.split(|unit| *unit == 0) {
        if value.is_empty() {
            break;
        }
        allowed.push(String::from_utf16(value).context("invalid AllowedAgents UTF-16")?);
    }
    Ok(allowed)
}

pub(super) fn read() -> Result<Policy> {
    resolve(read_value)
}

fn read_value(hive: Hive, name: &str, kind: u32) -> Result<Option<Vec<u8>>> {
    use windows_sys::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS,
    };
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
    };

    let root = match hive {
        Hive::Machine => HKEY_LOCAL_MACHINE,
        Hive::User => HKEY_CURRENT_USER,
    };
    let key: Vec<_> = r"Software\Policies\Microsoft\IntelligentTerminal"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let name_wide: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
    let mut size = 0;
    // SAFETY: the NUL-terminated names and byte-count output live across the call.
    let result = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            name_wide.as_ptr(),
            kind,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if matches!(result, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) {
        return Ok(None);
    }
    if result != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(result as i32))
            .with_context(|| format!("cannot read {name} policy ({hive:?})"));
    }
    if size == 0 || size > 65536 {
        bail!("invalid {name} policy size ({hive:?})");
    }
    let mut bytes = vec![0; size as usize];
    // SAFETY: the allocated buffer covers the supplied byte count. A concurrent
    // policy edit fails explicitly rather than falling back to an allow state.
    let result = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            name_wide.as_ptr(),
            kind,
            std::ptr::null_mut(),
            bytes.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(result as i32))
            .with_context(|| format!("cannot read {name} policy ({hive:?})"));
    }
    bytes.truncate(size as usize);
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn multi_sz(value: &str) -> Vec<u8> {
        value.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn absent_and_empty_policies_are_distinct() {
        let absent = resolve(|_, _, _| Ok(None)).unwrap();
        assert!(absent.check("copilot").is_ok());
        let empty =
            resolve(|_, name, _| Ok((name == "AllowedAgents").then(|| multi_sz("\0\0")))).unwrap();
        assert!(!empty.agent_allowed("copilot"));
        assert!(empty.check("copilot").is_err());
    }

    #[test]
    fn machine_policy_wins_and_user_fallback_is_per_value() {
        let policy = resolve(|hive, name, _| {
            Ok(match (hive, name) {
                (Hive::Machine, "AllowedAgents") => Some(multi_sz("CoPiLoT\0claude\0\0")),
                (Hive::User, "AllowedAgents") => panic!("machine allowlist must win"),
                (Hive::User, "AllowAutomaticApproval") => Some(0u32.to_le_bytes().to_vec()),
                _ => None,
            })
        })
        .unwrap();
        assert!(policy.agent_allowed("copilot"));
        assert!(policy.agent_allowed("claude"));
        assert!(!policy.agent_allowed("codex"));
        assert!(policy.check("copilot").is_err());
    }

    #[test]
    fn malformed_or_unreadable_policy_does_not_enable_launch() {
        assert!(resolve(|_, _, _| bail!("access denied")).is_err());
        for bytes in [
            vec![],
            vec![0],
            multi_sz("copilot\0"),
            vec![0, 216, 0, 0, 0, 0],
        ] {
            assert!(parse_allowlist(&bytes).is_err());
        }
        assert!(
            resolve(|_, name, _| Ok((name == "AllowAutomaticApproval").then(|| vec![1]))).is_err()
        );
    }
}
