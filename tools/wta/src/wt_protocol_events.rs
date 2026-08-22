/// Publish raw JSON events to Windows Terminal in submission order.
pub fn send(json_payload: String) {
    let _ = publisher_sender().send(json_payload);
}

fn publisher_sender() -> &'static std::sync::mpsc::Sender<String> {
    static SENDER: std::sync::OnceLock<std::sync::mpsc::Sender<String>> =
        std::sync::OnceLock::new();
    SENDER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::Builder::new()
            .name("wt-event-publisher".into())
            .spawn(move || {
                while let Ok(payload) = rx.recv() {
                    publish_blocking(&payload);
                }
            })
            .expect("spawn wt-event-publisher thread");
        tx
    })
}

fn publish_command(exe: &std::path::Path) -> std::process::Command {
    let mut command = std::process::Command::new(exe);
    command.arg("publish").arg("--stdin");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::piped());
    command
}

#[derive(Debug)]
enum PublishError {
    Spawn(std::io::Error),
    MissingStdin,
    Write(std::io::Error),
    Wait(std::io::Error),
}

fn execute_publish(
    command: &mut std::process::Command,
    json_payload: &[u8],
) -> Result<std::process::ExitStatus, PublishError> {
    use std::io::Write;

    let mut child = command.spawn().map_err(PublishError::Spawn)?;
    let write_result = match child.stdin.take() {
        Some(mut stdin) => stdin.write_all(json_payload).map_err(PublishError::Write),
        None => Err(PublishError::MissingStdin),
    };
    if let Err(error) = write_result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }

    child.wait().map_err(PublishError::Wait)
}

fn publish_blocking(json_payload: &str) {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|directory| directory.join("wtcli.exe")))
        .filter(|path| path.exists())
        .unwrap_or_else(|| std::path::PathBuf::from("wtcli.exe"));
    let payload_bytes = json_payload.len();
    let event_method_cache = std::sync::OnceLock::new();
    let event_method = || {
        event_method_cache.get_or_init(|| {
            serde_json::from_str::<serde_json::Value>(json_payload)
                .ok()
                .and_then(|event| event.get("method")?.as_str().map(str::to_owned))
                .unwrap_or_else(|| "<unknown>".to_owned())
        })
    };
    let mut command = publish_command(&exe);
    match execute_publish(&mut command, json_payload.as_bytes()) {
        Ok(status) if !status.success() => {
            tracing::warn!(
                target: "wt_protocol",
                ?status,
                payload_bytes,
                event_method = event_method(),
                "wtcli publish failed"
            );
        }
        Err(PublishError::Spawn(error)) => {
            tracing::warn!(
                target: "wt_protocol",
                %error,
                payload_bytes,
                event_method = event_method(),
                "failed to start wtcli publish"
            );
        }
        Err(PublishError::MissingStdin) => {
            tracing::warn!(
                target: "wt_protocol",
                payload_bytes,
                event_method = event_method(),
                "wtcli publish stdin was not piped"
            );
        }
        Err(PublishError::Write(error)) => {
            tracing::warn!(
                target: "wt_protocol",
                %error,
                payload_bytes,
                event_method = event_method(),
                "failed writing wtcli publish payload"
            );
        }
        Err(PublishError::Wait(error)) => {
            tracing::warn!(
                target: "wt_protocol",
                %error,
                payload_bytes,
                event_method = event_method(),
                "failed waiting for wtcli publish"
            );
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn publish_command_selects_stdin_transport() {
        let command = super::publish_command(std::path::Path::new("wtcli.exe"));
        let arguments: Vec<_> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();

        assert_eq!(arguments, ["publish", "--stdin"]);
    }

    #[cfg(windows)]
    #[test]
    fn execute_publish_writes_and_closes_large_stdin_payload() {
        let capture_path = std::env::temp_dir().join(format!(
            "wta-publish-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut command = std::process::Command::new("powershell.exe");
        command
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$source = [Console]::OpenStandardInput(); \
                 $destination = [IO.File]::Create($env:WTA_TEST_CAPTURE); \
                 try { $source.CopyTo($destination) } finally { $destination.Dispose() }",
            ])
            .env("WTA_TEST_CAPTURE", &capture_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let payload = format!(
            r#"{{"type":"event","method":"agent_status","params":{{"body":"{}"}}}}"#,
            "x".repeat(128 * 1024)
        );

        let status = super::execute_publish(&mut command, payload.as_bytes())
            .expect("fake wtcli process must accept the payload and observe EOF");
        let captured = std::fs::read(&capture_path).expect("fake wtcli must capture stdin");
        let _ = std::fs::remove_file(&capture_path);

        assert!(status.success());
        assert_eq!(captured, payload.as_bytes());
    }
}
