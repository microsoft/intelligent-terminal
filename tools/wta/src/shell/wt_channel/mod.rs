mod cli_channel;

pub(crate) use cli_channel::resolve_wtcli_path;
pub use cli_channel::spawn_wtcli_split_then_focus_with_callback;
pub use cli_channel::CliChannel;

/// Channel for communicating with the Windows Terminal protocol server.
#[async_trait::async_trait]
pub trait WtChannel: Send + Sync {
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value>;

    async fn wait_for_connection(&self, _session_id: &str) -> anyhow::Result<()> {
        anyhow::bail!("waiting for pane connection is not supported by this channel")
    }

    fn is_available(&self) -> bool;
}
