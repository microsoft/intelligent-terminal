use anyhow::{bail, Context, Result};
use serde_json::json;

use super::args::Command;
use crate::shell::wt_channel::{CliChannel, WtChannel};

pub(crate) async fn run(command: Command, json_mode: bool) -> Result<()> {
    match command {
        Command::Info => run_info_mode().await,
        Command::TestPipe => run_test_pipe().await,
        Command::ListWindows => {
            let result = wt_call("list_windows", json!({})).await?;
            print_output(&result, json_mode, format_windows_human);
            Ok(())
        }
        Command::ListTabs { window_id } => {
            let channel = connect_channel().await?;
            let result = list_tabs(&channel, window_id.as_deref()).await?;
            print_output(&result, json_mode, format_tabs_human);
            Ok(())
        }
        Command::ListPanes { tab_id, window_id } => {
            let channel = connect_channel().await?;
            let result = list_panes(&channel, tab_id.as_deref(), window_id.as_deref()).await?;
            print_output(&result, json_mode, format_panes_human);
            Ok(())
        }
        Command::NewTab {
            command,
            cwd,
            title,
        } => {
            let mut params = json!({});
            if let Some(c) = command {
                params["command"] = json!(c);
            }
            if let Some(d) = cwd {
                params["cwd"] = json!(d);
            }
            if let Some(t) = title {
                params["title"] = json!(t);
            }
            let result = wt_call("create_tab", params).await?;
            print_output(&result, json_mode, format_created_tab);
            Ok(())
        }
        Command::SplitPane {
            target,
            horizontal,
            vertical,
            size,
            command,
        } => {
            let channel = connect_channel().await?;
            let pane_id = resolve_pane_id(&channel, &target).await?;
            let split_dir = if horizontal {
                "horizontal"
            } else if vertical {
                "vertical"
            } else {
                "automatic"
            };
            let mut params = json!({
                "session_id": pane_id,
                "direction": split_dir,
            });
            if let Some(s) = size {
                params["size"] = json!(s);
            }
            if let Some(c) = command {
                params["command"] = json!(c);
            }
            let result = channel.request("split_pane", params).await?;
            print_output(&result, json_mode, format_created_pane);
            Ok(())
        }
        Command::CapturePane {
            target,
            max_lines,
            last_prompt,
        } => {
            let channel = connect_channel().await?;
            let pane_id = resolve_pane_id(&channel, &target).await?;
            let mut params = json!({ "session_id": pane_id });
            if let Some(n) = max_lines {
                params["max_lines"] = json!(n);
            }
            if last_prompt {
                params["source"] = json!("last_prompt");
            }
            let result = channel.request("read_pane_output", params).await?;
            if json_mode {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else if let Some(output) = result.get("content").and_then(|v| v.as_str()) {
                print!("{}", output);
            }
            Ok(())
        }
        Command::KillPane { target } => {
            let channel = connect_channel().await?;
            let pane_id = resolve_pane_id(&channel, &target).await?;
            channel
                .request("close_pane", json!({ "session_id": pane_id }))
                .await?;
            if !json_mode {
                println!("{}", t!("output.pane_closed", pane_id = pane_id));
            }
            Ok(())
        }
        Command::ActivePane => {
            let result = wt_call("get_active_pane", json!({})).await?;
            print_output(&result, json_mode, format_active_pane);
            Ok(())
        }
        Command::PaneStatus { target } => {
            let channel = connect_channel().await?;
            let pane_id = resolve_pane_id(&channel, &target).await?;
            let result = channel
                .request("get_process_status", json!({ "session_id": pane_id }))
                .await?;
            print_output(&result, json_mode, format_pane_status);
            Ok(())
        }
        Command::WaitFor {
            target,
            interval,
            timeout,
        } => run_wait_for(&target, interval, timeout, json_mode).await,
        Command::PipeId => run_pipe_id(json_mode),
        Command::SetEnv { shell } => run_set_env(&shell),
        Command::Listen { target } => run_listen(target.as_deref()).await,
        _ => bail!("unsupported command for WT CLI handler"),
    }
}

pub(crate) async fn run_test_pipe() -> Result<()> {
    println!("Connecting to Windows Terminal protocol...");
    let channel = connect_channel().await?;
    println!("Connected and authenticated!\n");

    let result: serde_json::Value = channel.request("list_windows", json!({})).await?;
    println!("list_windows:");
    println!("{}\n", serde_json::to_string_pretty(&result)?);

    let result: serde_json::Value = channel.request("get_capabilities", json!({})).await?;
    println!("get_capabilities:");
    println!("{}", serde_json::to_string_pretty(&result)?);

    Ok(())
}

/// Show Windows Terminal protocol connection info and pane identity.
pub(crate) async fn run_info_mode() -> Result<()> {
    println!("Windows Terminal Protocol Info");
    println!("========================================");

    let clsid = match std::env::var("WT_COM_CLSID") {
        Ok(v) => v,
        Err(_) => {
            println!("  Status: Not running inside Windows Terminal");
            println!("  (WT_COM_CLSID not set)");
            return Ok(());
        }
    };

    println!("  COM CLSID: {}", clsid);
    println!("  Source: WT_COM_CLSID env var");
    println!();

    let channel = match CliChannel::connect().await {
        Ok(ch) => ch,
        Err(e) => {
            println!("  Connection failed: {}", e);
            return Ok(());
        }
    };

    let our_pid = std::process::id();
    let mut pane_info: Option<(String, String, String)> = None;
    let mut total_windows = 0u32;
    let mut total_tabs = 0u32;
    let mut total_panes = 0u32;

    if let Ok(windows) = channel.request("list_windows", json!({})).await {
        if let Some(windows_arr) = windows.get("windows").and_then(|v| v.as_array()) {
            total_windows = windows_arr.len() as u32;

            for win in windows_arr {
                let window_id = match win.get("window_id").and_then(|v| v.as_str()) {
                    Some(id) => id,
                    None => continue,
                };

                if let Ok(tabs) = channel
                    .request("list_tabs", json!({ "window_id": window_id }))
                    .await
                {
                    if let Some(tabs_arr) = tabs.get("tabs").and_then(|v| v.as_array()) {
                        total_tabs += tabs_arr.len() as u32;

                        for tab in tabs_arr {
                            let tab_id_str = match tab.get("tab_id") {
                                Some(serde_json::Value::String(s)) => s.clone(),
                                Some(serde_json::Value::Number(n)) => n.to_string(),
                                _ => continue,
                            };

                            if let Ok(panes) = channel
                                .request("list_panes", json!({ "tab_id": tab_id_str }))
                                .await
                            {
                                if let Some(panes_arr) =
                                    panes.get("panes").and_then(|v| v.as_array())
                                {
                                    total_panes += panes_arr.len() as u32;

                                    for pane in panes_arr {
                                        if let Some(pid) = pane.get("pid").and_then(|v| v.as_u64())
                                        {
                                            if pid == our_pid as u64 {
                                                let pane_id = match pane.get("session_id") {
                                                    Some(serde_json::Value::String(s)) => s.clone(),
                                                    Some(serde_json::Value::Number(n)) => {
                                                        n.to_string()
                                                    }
                                                    _ => "?".to_string(),
                                                };
                                                pane_info = Some((
                                                    pane_id,
                                                    tab_id_str.clone(),
                                                    window_id.to_string(),
                                                ));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some((pane_id, tab_id, window_id)) = pane_info {
        println!("Current Pane (PID {}):", our_pid);
        println!("  Window ID: {}", window_id);
        println!("  Tab ID:    {}", tab_id);
        println!("  Pane ID:   {}", pane_id);
    } else {
        println!("Current Pane (PID {}): not found in WT pane list", our_pid);
    }

    println!();
    println!("Summary:");
    println!(
        "  Windows: {}, Tabs: {}, Panes: {}",
        total_windows, total_tabs, total_panes
    );

    Ok(())
}

async fn connect_channel() -> Result<CliChannel> {
    CliChannel::connect().await
}

async fn wt_call(method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    let channel = connect_channel().await?;
    channel.request(method, params).await
}

async fn resolve_pane_id(channel: &CliChannel, target: &Option<String>) -> Result<String> {
    match target {
        Some(id) => Ok(id.clone()),
        None => {
            let result = channel.request("get_active_pane", json!({})).await?;
            result
                .get("session_id")
                .and_then(|v| match v {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    _ => None,
                })
                .ok_or_else(|| anyhow::anyhow!("{}", t!("error.no_active_pane")))
        }
    }
}

async fn list_tabs(channel: &dyn WtChannel, window_id: Option<&str>) -> Result<serde_json::Value> {
    let mut params = json!({});
    if let Some(window_id) = window_id {
        params["window_id"] = json!(window_id);
    }
    channel.request("list_tabs", params).await
}

async fn list_panes(
    channel: &dyn WtChannel,
    tab_id: Option<&str>,
    window_id: Option<&str>,
) -> Result<serde_json::Value> {
    // Let wtcli resolve defaults and retain the window-local tab scope.
    let mut params = json!({});
    if let Some(tab_id) = tab_id {
        params["tab_id"] = json!(tab_id);
    }
    if let Some(window_id) = window_id {
        params["window_id"] = json!(window_id);
    }
    channel.request("list_panes", params).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct QueryChannel {
        requests: Mutex<Vec<(String, serde_json::Value)>>,
        fail_method: Option<&'static str>,
    }

    impl QueryChannel {
        fn new() -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                fail_method: None,
            }
        }

        fn assert_request(&self, method: &str, params: serde_json::Value) {
            assert_eq!(
                *self.requests.lock().unwrap(),
                [(method.to_owned(), params)]
            );
        }
    }

    #[async_trait::async_trait]
    impl WtChannel for QueryChannel {
        async fn request(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<serde_json::Value> {
            self.requests
                .lock()
                .unwrap()
                .push((method.to_owned(), params.clone()));
            if self.fail_method == Some(method) {
                bail!("native {method} failed");
            }
            match method {
                "list_windows" => Ok(json!({"windows": [
                    {"window_id": 2, "is_focused": false},
                    {"window_id": 1, "is_focused": true}
                ]})),
                "list_tabs" => Ok(json!({"tabs": [
                    {"tab_id": 0, "is_active": false},
                    {"tab_id": 1, "is_active": true}
                ]})),
                "list_panes" => {
                    let tab_id = params["tab_id"].as_str().unwrap_or("0");
                    let window_id = params["window_id"]
                        .as_str()
                        .or_else(|| params.get("tab_id").is_none().then_some("2"));
                    let panes: Vec<_> = [(2, 0), (1, 0), (2, 1), (1, 1)]
                        .into_iter()
                        .filter(|(window, tab)| {
                            window_id.is_none_or(|id| id == window.to_string())
                                && tab_id == tab.to_string()
                        })
                        .map(|(window, tab)| json!({"window_id": window, "tab_id": tab}))
                        .collect();
                    Ok(json!({"panes": panes}))
                }
                _ => bail!("unexpected request {method}"),
            }
        }

        fn is_available(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn list_tabs_delegates_default_window_discovery_to_native_cli() {
        let channel = QueryChannel::new();
        let result = list_tabs(&channel, None).await.unwrap();
        assert_eq!(result["tabs"][0]["tab_id"], 0);
        channel.assert_request("list_tabs", json!({}));
    }

    #[tokio::test]
    async fn list_tabs_preserves_explicit_window_ids() {
        for window_id in ["1", "18446744073709551615"] {
            let channel = QueryChannel::new();
            list_tabs(&channel, Some(window_id)).await.unwrap();
            channel.assert_request("list_tabs", json!({"window_id": window_id}));
        }
    }

    #[tokio::test]
    async fn list_panes_delegates_default_window_and_tab_to_native_cli() {
        let channel = QueryChannel::new();
        let result = list_panes(&channel, None, None).await.unwrap();
        assert_eq!(result["panes"], json!([{"window_id": 2, "tab_id": 0}]));
        channel.assert_request("list_panes", json!({}));
    }

    #[tokio::test]
    async fn list_panes_keeps_explicit_window_scope_with_default_or_explicit_tab() {
        for tab_id in [None, Some("1")] {
            let channel = QueryChannel::new();
            let result = list_panes(&channel, tab_id, Some("1")).await.unwrap();
            let mut params = json!({"window_id": "1"});
            let expected_tab = if let Some(tab_id) = tab_id {
                params["tab_id"] = json!(tab_id);
                1
            } else {
                0
            };
            assert_eq!(
                result["panes"],
                json!([{"window_id": 1, "tab_id": expected_tab}])
            );
            channel.assert_request("list_panes", params);
        }
    }

    #[tokio::test]
    async fn list_panes_keeps_explicit_tab_only_queries_cross_window() {
        let channel = QueryChannel::new();
        let result = list_panes(&channel, Some("0"), None).await.unwrap();
        assert_eq!(
            result["panes"],
            json!([{"window_id": 2, "tab_id": 0}, {"window_id": 1, "tab_id": 0}])
        );
        channel.assert_request("list_panes", json!({"tab_id": "0"}));
    }

    #[tokio::test]
    async fn list_query_errors_propagate_without_fallback() {
        for method in ["list_tabs", "list_panes"] {
            let mut channel = QueryChannel::new();
            channel.fail_method = Some(method);
            let error = if method == "list_tabs" {
                list_tabs(&channel, Some("1")).await.unwrap_err()
            } else {
                list_panes(&channel, Some("0"), Some("1"))
                    .await
                    .unwrap_err()
            };
            assert_eq!(error.to_string(), format!("native {method} failed"));
            assert_eq!(channel.requests.lock().unwrap().len(), 1);
        }
    }
}

fn print_output(val: &serde_json::Value, json_mode: bool, formatter: fn(&serde_json::Value)) {
    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(val).unwrap_or_else(|_| val.to_string())
        );
    } else {
        formatter(val);
    }
}

fn format_windows_human(val: &serde_json::Value) {
    if let Some(windows) = val.get("windows").and_then(|v| v.as_array()) {
        if windows.is_empty() {
            println!("{}", t!("output.no_windows"));
            return;
        }
        println!("{}", t!("output.header.windows"));
        for w in windows {
            let id = json_str_or_num(w, "window_id");
            let title = w.get("title").and_then(|v| v.as_str()).unwrap_or("-");
            let focused = w
                .get("is_focused")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            println!(
                "{:<12} {:<30} {}",
                id,
                title,
                if focused { "*" } else { "" }
            );
        }
    } else {
        println!("{}", serde_json::to_string_pretty(val).unwrap_or_default());
    }
}

fn format_tabs_human(val: &serde_json::Value) {
    if let Some(tabs) = val.get("tabs").and_then(|v| v.as_array()) {
        if tabs.is_empty() {
            println!("{}", t!("output.no_tabs"));
            return;
        }
        println!("{}", t!("output.header.tabs"));
        for tab in tabs {
            let id = json_str_or_num(tab, "tab_id");
            let title = tab.get("title").and_then(|v| v.as_str()).unwrap_or("-");
            let focused = tab
                .get("is_active")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            println!(
                "{:<10} {:<30} {}",
                id,
                title,
                if focused { "*" } else { "" }
            );
        }
    } else {
        println!("{}", serde_json::to_string_pretty(val).unwrap_or_default());
    }
}

fn format_panes_human(val: &serde_json::Value) {
    if let Some(panes) = val.get("panes").and_then(|v| v.as_array()) {
        if panes.is_empty() {
            println!("{}", t!("output.no_panes"));
            return;
        }
        println!("{}", t!("output.header.panes"));
        for pane in panes {
            let id = json_str_or_num(pane, "session_id");
            let pid = pane
                .get("pid")
                .and_then(|v| v.as_u64())
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".to_string());
            let active = pane
                .get("is_active")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let size = pane.get("size");
            let rows = size
                .and_then(|s| s.get("rows"))
                .and_then(|v| v.as_u64())
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".to_string());
            let cols = size
                .and_then(|s| s.get("columns"))
                .and_then(|v| v.as_u64())
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".to_string());
            println!(
                "{:<10} {:<8} {:<8} {:<10} {}",
                id,
                pid,
                if active { "*" } else { "" },
                rows,
                cols
            );
        }
    } else {
        println!("{}", serde_json::to_string_pretty(val).unwrap_or_default());
    }
}

fn format_active_pane(val: &serde_json::Value) {
    let id = json_str_or_num(val, "session_id");
    let tab = json_str_or_num(val, "tab_id");
    let win = json_str_or_num(val, "window_id");
    println!(
        "{}",
        t!("output.active_pane", pane = id, tab = tab, window = win)
    );
}

fn format_pane_status(val: &serde_json::Value) {
    let state = val
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let running = state == "running";
    let exit_code = val
        .get("exit_code")
        .and_then(|v| v.as_i64())
        .map(|n| n.to_string())
        .unwrap_or_else(|| "-".to_string());
    let pid = val
        .get("pid")
        .and_then(|v| v.as_u64())
        .map(|n| n.to_string())
        .unwrap_or_else(|| "-".to_string());
    if running {
        println!("{}", t!("output.pane_running", pid = pid));
    } else {
        println!("{}", t!("output.pane_exited", code = exit_code, pid = pid));
    }
}

fn format_created_tab(val: &serde_json::Value) {
    let tab_id = json_str_or_num(val, "tab_id");
    let pane_id = json_str_or_num(val, "session_id");
    println!(
        "{}",
        t!("output.created_tab", tab_id = tab_id, pane_id = pane_id)
    );
}

fn format_created_pane(val: &serde_json::Value) {
    let pane_id = json_str_or_num(val, "session_id");
    println!("{}", t!("output.created_pane", pane_id = pane_id));
}

pub(crate) fn json_str_or_num(val: &serde_json::Value, key: &str) -> String {
    match val.get(key) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => "-".to_string(),
    }
}

async fn run_wait_for(target: &str, interval: u64, timeout: u64, json_mode: bool) -> Result<()> {
    let wtcli = crate::shell::wt_channel::resolve_wtcli_path();
    let interval_str = interval.to_string();
    let timeout_str = timeout.to_string();
    let output = tokio::process::Command::new(&wtcli)
        .args([
            "--json",
            "wait-for",
            "-t",
            target,
            "--interval",
            &interval_str,
            "--timeout",
            &timeout_str,
        ])
        .output()
        .await
        .with_context(|| t!("error.wtcli_wait_for_spawn").into_owned())?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{}",
            t!("error.wtcli_wait_for_failed", stderr = stderr.trim())
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if !trimmed.is_empty() {
        let val: serde_json::Value = serde_json::from_str(trimmed)
            .with_context(|| t!("error.wtcli_wait_for_parse").into_owned())?;
        print_output(&val, json_mode, format_pane_status);
    }
    Ok(())
}

fn run_pipe_id(json_mode: bool) -> Result<()> {
    let clsid = std::env::var("WT_COM_CLSID")
        .map_err(|_| anyhow::anyhow!("{}", t!("error.wt_com_clsid_not_set")))?;
    if json_mode {
        let val = json!({ "connection_id": clsid, "env": "WT_COM_CLSID" });
        println!("{}", serde_json::to_string_pretty(&val)?);
    } else {
        println!("{}", clsid);
    }
    Ok(())
}

fn run_set_env(shell_type: &str) -> Result<()> {
    let clsid = std::env::var("WT_COM_CLSID")
        .map_err(|_| anyhow::anyhow!("{}", t!("error.wt_com_clsid_not_set")))?;

    match shell_type {
        "bash" | "sh" | "zsh" => {
            println!("export WT_COM_CLSID='{}'", clsid);
            eprintln!("# Run: eval \"$(wta set-env)\"");
        }
        "powershell" | "pwsh" | "ps" => {
            println!("$env:WT_COM_CLSID = '{}'", clsid);
            eprintln!("# Run: wta set-env -s powershell | Invoke-Expression");
        }
        "cmd" => {
            println!("set WT_COM_CLSID={}", clsid);
            eprintln!("REM Run in a for /f loop or copy-paste");
        }
        "fish" => {
            println!("set -gx WT_COM_CLSID '{}'", clsid);
            eprintln!("# Run: wta set-env -s fish | source");
        }
        other => bail!("{}", t!("error.unknown_shell_type", shell = other)),
    }

    Ok(())
}

async fn run_listen(pane_filter: Option<&str>) -> Result<()> {
    let channel = connect_channel().await?;
    let arc_channel = std::sync::Arc::new(channel);

    let mut event_rx = arc_channel.subscribe_events();
    arc_channel.start_reader().await;

    let _ = arc_channel.request("get_capabilities", json!({})).await;

    eprintln!("Connected. Listening for events... (Ctrl+C to stop)");
    if let Some(pane) = pane_filter {
        eprintln!("Filtering: pane_id={}", pane);
    }

    while let Some(msg) = event_rx.recv().await {
        if msg.get("type").and_then(|v| v.as_str()) != Some("event") {
            continue;
        }

        if let Some(filter) = pane_filter {
            let pane_id = msg
                .get("params")
                .and_then(|p| p.get("session_id"))
                .and_then(|v| v.as_str());
            if pane_id != Some(filter) {
                continue;
            }
        }

        println!("{}", serde_json::to_string(&msg).unwrap_or_default());
    }

    eprintln!("Event stream closed.");
    Ok(())
}
