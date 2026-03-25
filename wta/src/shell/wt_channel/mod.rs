pub mod pipe_channel;
pub(crate) mod types;
pub mod vt_channel;

pub use pipe_channel::PipeChannel;
pub use vt_channel::{discover_connection_info, ConnectionInfo, DiscoverySource};

/// Channel for communicating with the Windows Terminal protocol server.
///
/// Methods map 1:1 to the WT protocol: "list_windows", "create_tab",
/// "read_pane_output", etc. Params and results are raw JSON values.
#[async_trait::async_trait]
pub trait WtChannel: Send + Sync {
    /// Send a protocol request and return the result.
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value>;

    /// Whether the channel is connected and ready.
    fn is_available(&self) -> bool;
}
