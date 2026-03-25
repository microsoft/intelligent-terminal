use std::io::{IsTerminal, Read, Write};
use std::sync::mpsc;
use std::time::Duration;

/// Connection info discovered via VT sequence or environment variables.
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub pipe_name: String,
    pub token: String,
    pub source: DiscoverySource,
}

#[derive(Debug, Clone)]
pub enum DiscoverySource {
    /// Discovered via OSC 9001 VT escape sequence
    VtOsc,
    /// Read from WT_PIPE_NAME / WT_MCP_TOKEN environment variables
    EnvVar,
}

/// Discover WT protocol connection info with fallback chain:
/// 1. Environment variable WT_PIPE_NAME (always set by WT via SetEnvironmentVariableW)
/// 2. VT OSC 9001 discover sequence (legacy fallback, only when stdout is a TTY)
/// 3. None (not running inside Windows Terminal)
pub fn discover_connection_info() -> Option<ConnectionInfo> {
    // Try environment variables first — WT injects WT_PIPE_NAME into the
    // process environment so all child panes inherit it automatically.
    if let Ok(pipe_name) = std::env::var("WT_PIPE_NAME") {
        let token = std::env::var("WT_MCP_TOKEN").unwrap_or_default();
        return Some(ConnectionInfo {
            pipe_name,
            token,
            source: DiscoverySource::EnvVar,
        });
    }

    // Fall back to VT discovery (only works when stdout is a real terminal)
    if std::io::stdout().is_terminal() {
        if let Some(info) = try_vt_discover() {
            return Some(info);
        }
    }

    None
}

/// Try to discover connection info via VT OSC 9001 escape sequence.
///
/// Sends `\x1b]9001;WtaReq;{"method":"discover"}\x07` to stdout,
/// reads the response `\x1b]9001;WtaRes;{json}\x1b\\` from stdin,
/// and parses pipe name + token from the JSON response.
fn try_vt_discover() -> Option<ConnectionInfo> {
    // MCP stdio mode uses stdin/stdout as a protocol transport, not a real terminal.
    // Emitting VT discovery bytes there corrupts the stream and breaks handshake.
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return None;
    }

    // Enable raw mode so we can read the terminal's response from stdin
    crossterm::terminal::enable_raw_mode().ok()?;

    let result = try_vt_discover_inner();

    // Always restore normal mode
    let _ = crossterm::terminal::disable_raw_mode();

    result
}

fn try_vt_discover_inner() -> Option<ConnectionInfo> {
    // Write the OSC discover request to stdout
    let mut stdout = std::io::stdout();
    stdout
        .write_all(b"\x1b]9001;WtaReq;{\"method\":\"discover\"}\x07")
        .ok()?;
    stdout.flush().ok()?;

    // Read response from stdin on a blocking thread with timeout.
    // The response format is: \x1b]9001;WtaRes;{json}\x1b\\
    let (tx, rx) = mpsc::channel::<Vec<u8>>();

    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = Vec::with_capacity(512);
        let mut byte = [0u8; 1];

        // Read until we see the ST (String Terminator): \x1b\\
        loop {
            match stdin.read(&mut byte) {
                Ok(1) => {
                    buf.push(byte[0]);
                    // Check for ST: last two bytes are \x1b and '\\'
                    if buf.len() >= 2 && buf[buf.len() - 2] == 0x1b && buf[buf.len() - 1] == b'\\' {
                        let _ = tx.send(buf);
                        return;
                    }
                    // Safety: bail if response is unreasonably long
                    if buf.len() > 4096 {
                        return;
                    }
                }
                _ => return,
            }
        }
    });

    // Wait up to 2 seconds for the response
    let raw = rx.recv_timeout(Duration::from_secs(2)).ok()?;

    // Parse: find "WtaRes;" then extract JSON until ST
    let raw_str = String::from_utf8_lossy(&raw);
    let marker = "WtaRes;";
    let json_start = raw_str.find(marker)? + marker.len();
    // JSON ends before the final \x1b\\
    let json_end = raw_str.len().checked_sub(2)?;
    if json_start >= json_end {
        return None;
    }
    let json_str = &raw_str[json_start..json_end];

    // Parse the JSON response
    let resp: serde_json::Value = serde_json::from_str(json_str).ok()?;

    let status = resp.get("status")?.as_str()?;
    if status != "ok" {
        return None;
    }

    let pipe_name = resp.get("pipe")?.as_str()?.to_string();
    let token = resp
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Some(ConnectionInfo {
        pipe_name,
        token,
        source: DiscoverySource::VtOsc,
    })
}
