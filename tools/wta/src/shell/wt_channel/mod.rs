mod cli_channel;

pub use cli_channel::CliChannel;
pub use cli_channel::spawn_wtcli_focus_pane;
pub use cli_channel::spawn_wtcli_focus_pane_with_callback;
pub use cli_channel::spawn_wtcli_split_then_focus_with_callback;
pub use cli_channel::FocusPaneFailureReason;
pub(crate) use cli_channel::resolve_wtcli_path;

/// Channel for communicating with the Windows Terminal protocol server.
#[async_trait::async_trait]
pub trait WtChannel: Send + Sync {
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value>;

    fn is_available(&self) -> bool;
}
