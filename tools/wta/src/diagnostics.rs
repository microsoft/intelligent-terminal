// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

//! Allowlisted provenance only. Never log the source JSON or environment.

use serde_json::{json, Value};

/// Logging schema only, never a routing/session validity check. ACP session IDs
/// may be opaque; do not log or hash those values (including environment data).
pub(crate) fn identity(value: Option<&str>) -> String {
    let Some(value) = value else {
        return "absent".into();
    };
    let guid = value
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
        .unwrap_or(value);
    if guid.len() == 36 {
        if let Ok(id) = uuid::Uuid::parse_str(guid) {
            return id.hyphenated().to_string();
        }
    }
    if numeric(value, 20) {
        if let Ok(id) = value.parse::<u64>() {
            return id.to_string();
        }
    }
    "unsupported_redacted".into()
}

// A fixed, versioned PE section lets the report read the actual WTA image's
// build identity without executing a probe or guessing from the installed copy.
// Fields are NUL-terminated: signature[16], Cargo version[32], commit[65].
#[used]
#[cfg_attr(windows, unsafe(link_section = ".wtadiag"))]
static EMBEDDED_BUILD_IDENTITY: [u8; 128] = {
    let mut record = [0; 128];
    let marker = b"WTA-DIAG-1";
    let version = env!("CARGO_PKG_VERSION").as_bytes();
    let commit = env!("WTA_BUILD_COMMIT").as_bytes();
    let mut i = 0;
    while i < marker.len() {
        record[i] = marker[i];
        i += 1;
    }
    i = 0;
    while i < version.len() && i < 31 {
        record[16 + i] = version[i];
        i += 1;
    }
    i = 0;
    while i < commit.len() && i < 64 {
        record[48 + i] = commit[i];
        i += 1;
    }
    record
};

pub(crate) fn retain_build_identity() {
    // Keep the section reachable under the release linker's /OPT:REF.
    std::hint::black_box(&EMBEDDED_BUILD_IDENTITY);
}

fn numeric(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && value.bytes().all(|b| b.is_ascii_digit())
}

fn version(value: &str) -> bool {
    if value.len() > 40 {
        return false;
    }
    let parts: Vec<_> = value.split('.').collect();
    (2..=4).contains(&parts.len()) && parts.iter().all(|part| numeric(part, 10))
}

fn package_identity(value: &str) -> bool {
    if value.len() > 256 {
        return false;
    }
    let parts: Vec<_> = value.split('_').collect();
    value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && parts.len() == 5
        && !parts[0].is_empty()
        && version(parts[1])
        && matches!(parts[2], "x64" | "x86" | "arm64" | "arm" | "neutral")
        && parts[4].len() == 13
}

/// Diagnostics are optional on old servers, and malformed metadata is never a
/// reason to reject a valid pane context. Output is rebuilt, not copied.
pub(crate) fn context_provenance(context: &Value) -> Value {
    let input = &context["diagnostics"];
    let identity = &input["server_identity"];
    let mut safe = json!({"status": if input.is_null() { "unavailable" } else { "invalid" }});
    let Some(pid) = identity["pid"]
        .as_u64()
        .filter(|pid| *pid > 0 && *pid <= u32::MAX as u64)
    else {
        return safe;
    };
    let Some(start) = identity["start_time_filetime"]
        .as_str()
        .filter(|s| numeric(s, 20) && s.parse::<u64>().is_ok_and(|ticks| ticks > 0))
    else {
        return safe;
    };
    safe["status"] = json!("available");
    safe["server_pid"] = json!(pid);
    safe["server_start_time_filetime"] = json!(start);
    safe["server_instance_id"] = json!(format!("{pid}-{start}"));
    for field in ["package_version", "product_version"] {
        if let Some(value) = identity[field].as_str().filter(|s| version(s)) {
            safe[field] = json!(value);
        }
    }
    if let Some(name) = identity["package_full_name"]
        .as_str()
        .filter(|s| package_identity(s))
    {
        safe["package_full_name"] = json!(name);
    }
    if let Some(request) = input["request_id"].as_u64().filter(|value| *value > 0) {
        safe["request_id"] = json!(request);
    }
    if let Some(caller) = input["caller_pid"]
        .as_u64()
        .filter(|value| *value <= u32::MAX as u64)
    {
        safe["caller_pid"] = json!(caller);
    }
    safe
}

#[cfg(windows)]
fn process_start(process: windows_sys::Win32::Foundation::HANDLE) -> Option<u64> {
    use windows_sys::Win32::{Foundation::FILETIME, System::Threading::GetProcessTimes};
    let mut created = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;
    // SAFETY: callers provide a valid borrowed process handle; all outputs are
    // initialized, writable FILETIMEs and remain alive throughout this call.
    if unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        None
    } else {
        Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }
}

pub(crate) fn current_process_start() -> Option<u64> {
    #[cfg(windows)]
    {
        // SAFETY: the pseudo-handle is always valid and must not be closed.
        process_start(unsafe { windows_sys::Win32::System::Threading::GetCurrentProcess() })
    }
    #[cfg(not(windows))]
    None
}

#[cfg(windows)]
pub(crate) fn log_connected_master(pipe: &tokio::net::windows::named_pipe::NamedPipeClient) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetNamedPipeServerProcessId(pipe: HANDLE, pid: *mut u32) -> i32;
    }
    let mut pid = 0;
    // SAFETY: the borrowed pipe remains open; pid is a writable DWORD.
    let known = unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) } != 0;
    let start = if known {
        // SAFETY: query-only access to the single actual pipe server PID.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            None
        } else {
            let result = process_start(process);
            // SAFETY: this function owns the OpenProcess handle.
            unsafe { CloseHandle(process) };
            result
        }
    } else {
        None
    };
    tracing::info!(target: "acp.terminal_context",
        helper_pid = std::process::id(),
        helper_start_time_filetime = current_process_start(),
        master_pid = known.then_some(pid), master_start_time_filetime = start,
        identity_source = "connected_named_pipe_server",
        "helper_master_identity");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_identity_schema_redacts_opaque_values_without_changing_input() {
        assert_eq!(identity(None), "absent");
        for input in [
            "",
            "PRIVATE_ENV_TOKEN=secret",
            r"C:\Users\PRIVATE_USER\secret.txt",
            "opaque-acp-session",
            "12345678123412341234123456789abc",
            "urn:uuid:12345678-1234-1234-1234-123456789abc",
            "18446744073709551616",
            "-1",
            "42\nPRIVATE_ENV_TOKEN",
        ] {
            let original = input.to_string();
            assert_eq!(identity(Some(input)), "unsupported_redacted");
            assert_eq!(input, original);
        }
        for (input, expected) in [
            ("0", "0"),
            ("42", "42"),
            ("18446744073709551615", "18446744073709551615"),
            ("00042", "42"),
            (
                "{12345678-1234-1234-1234-123456789ABC}",
                "12345678-1234-1234-1234-123456789abc",
            ),
            (
                "12345678-1234-1234-1234-123456789abc",
                "12345678-1234-1234-1234-123456789abc",
            ),
        ] {
            assert_eq!(identity(Some(input)), expected);
        }
    }

    #[test]
    fn diagnostics_embedded_build_record_is_fixed_and_bounded() {
        assert_eq!(&EMBEDDED_BUILD_IDENTITY[..11], b"WTA-DIAG-1\0");
        assert_eq!(
            &EMBEDDED_BUILD_IDENTITY[16..16 + env!("CARGO_PKG_VERSION").len()],
            env!("CARGO_PKG_VERSION").as_bytes()
        );
        assert_eq!(
            &EMBEDDED_BUILD_IDENTITY[48..48 + env!("WTA_BUILD_COMMIT").len()],
            env!("WTA_BUILD_COMMIT").as_bytes()
        );
        assert!(package_identity(
            "Microsoft.WindowsTerminal_1.2.3.4_x64__8wekyb3d8bbwe"
        ));
        assert!(!package_identity("PRIVATE_SECRET"));
    }

    #[test]
    fn diagnostics_old_and_malformed_metadata_do_not_change_context() {
        for diagnostics in [
            Value::Null,
            json!(true),
            json!([]),
            json!({"server_identity": {"pid": "secret"}}),
        ] {
            let context = json!({"content": "PRIVATE_CONTENT", "diagnostics": diagnostics});
            let before = context.clone();
            let safe = context_provenance(&context);
            assert_eq!(context, before);
            assert_ne!(safe["status"], "available");
            assert!(!safe.to_string().contains("PRIVATE"));
            assert!(!safe.to_string().contains("secret"));
        }
        assert_eq!(
            context_provenance(&json!({"content": "old"}))["status"],
            "unavailable"
        );
    }

    #[test]
    fn diagnostics_provenance_rebuilds_allowlist_and_bounds_values() {
        let context = json!({
            "content": "PRIVATE_OUTPUT",
            "diagnostics": {
                "request_id": 19, "caller_pid": 45, "secret": "PRIVATE_TOKEN",
                "server_identity": {
                    "pid": 123, "start_time_filetime": "134000000000000000",
                    "instance_id": "PRIVATE_FAKE", "package_version": "1.2.3.4",
                    "product_version": "PRIVATE_VERSION", "package_full_name": "PRIVATE\\PATH",
                    "environment": "PRIVATE_ENV", "command": "PRIVATE_COMMAND"
                }
            }
        });
        let safe = context_provenance(&context);
        assert_eq!(safe["status"], "available");
        assert_eq!(safe["server_instance_id"], "123-134000000000000000");
        assert_eq!(safe["request_id"], 19);
        assert_eq!(safe["caller_pid"], 45);
        assert!(!safe.to_string().contains("PRIVATE"));
        for invalid in [
            json!(0),
            json!(-1),
            json!(u64::MAX),
            json!("123"),
            json!(123.5),
        ] {
            let mut input = context.clone();
            input["diagnostics"]["server_identity"]["pid"] = invalid;
            assert_eq!(context_provenance(&input)["status"], "invalid");
        }
        for invalid in [
            "",
            "0",
            "18446744073709551616",
            "PRIVATE_START",
            "1".repeat(100).as_str(),
        ] {
            let mut input = context.clone();
            input["diagnostics"]["server_identity"]["start_time_filetime"] = json!(invalid);
            assert_eq!(context_provenance(&input)["status"], "invalid");
        }
    }
}
