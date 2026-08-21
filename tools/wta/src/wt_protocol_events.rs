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

fn publish_blocking(json_payload: &str) {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|directory| directory.join("wtcli.exe")))
        .filter(|path| path.exists())
        .unwrap_or_else(|| std::path::PathBuf::from("wtcli.exe"));
    let mut command = std::process::Command::new(exe);
    command.arg("publish").arg(json_payload);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null());
    match command.spawn() {
        Ok(mut child) => match child.wait() {
            Ok(status) if !status.success() => {
                tracing::warn!(target: "wt_protocol", ?status, "wtcli publish failed");
            }
            Err(error) => {
                tracing::warn!(target: "wt_protocol", %error, "failed waiting for wtcli publish");
            }
            _ => {}
        },
        Err(error) => {
            tracing::warn!(target: "wt_protocol", %error, "failed to start wtcli publish");
        }
    }
}
