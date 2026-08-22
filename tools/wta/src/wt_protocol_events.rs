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

fn publish_blocking(json_payload: &str) {
    use std::io::Write;

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
    match command.spawn() {
        Ok(mut child) => {
            match child.stdin.take() {
                Some(mut stdin) => {
                    if let Err(error) = stdin.write_all(json_payload.as_bytes()) {
                        tracing::warn!(
                            target: "wt_protocol",
                            %error,
                            payload_bytes,
                            event_method = event_method(),
                            "failed writing wtcli publish payload"
                        );
                    }
                }

                None => {
                    tracing::warn!(
                        target: "wt_protocol",
                        payload_bytes,
                        event_method = event_method(),
                        "wtcli publish stdin was not piped"
                    );
                }
            }

            match child.wait() {
                Ok(status) if !status.success() => {
                    tracing::warn!(
                        target: "wt_protocol",
                        ?status,
                        payload_bytes,
                        event_method = event_method(),
                        "wtcli publish failed"
                    );
                }
                Err(error) => {
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
        Err(error) => {
            tracing::warn!(
                target: "wt_protocol",
                %error,
                payload_bytes,
                event_method = event_method(),
                "failed to start wtcli publish"
            );
        }
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
}
