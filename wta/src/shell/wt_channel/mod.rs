mod cli_channel;
mod pipe_channel;
mod routed_channel;

pub use cli_channel::CliChannel;
pub use cli_channel::spawn_wtcli_focus_pane;
pub use pipe_channel::PipeChannel;
pub use routed_channel::RoutedChannel;
pub(crate) use cli_channel::resolve_wtcli_path;

/// Connection info discovered from environment variables.
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub pipe_name: String,
    pub token: String,
    pub source: DiscoverySource,
}

#[derive(Debug, Clone)]
pub enum DiscoverySource {
    VtOsc,
    ComClsid,
    /// WT inherited a duplex anonymous pipe pair into this process via
    /// STARTUPINFOEX PROC_THREAD_ATTRIBUTE_HANDLE_LIST. Handle values
    /// arrived in `WT_PROTOCOL_PIPE_R` / `WT_PROTOCOL_PIPE_W` (consumed).
    InheritedPipe,
}

/// Discover WT protocol connection info from the WT_COM_CLSID env var.
/// (The legacy WT_PIPE_NAME / WT_MCP_TOKEN fallback was vestigial — WT
/// never produced those vars — and has been removed.)
pub fn discover_connection_info() -> Option<ConnectionInfo> {
    if let Ok(clsid) = std::env::var("WT_COM_CLSID") {
        return Some(ConnectionInfo {
            pipe_name: clsid,
            token: String::new(),
            source: DiscoverySource::ComClsid,
        });
    }
    None
}

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
