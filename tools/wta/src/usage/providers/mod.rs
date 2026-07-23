mod claude;
mod codex;
mod copilot;
mod gemini;
mod opencode;

use super::UsageCost;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateUsagePolicy {
    /// Standard ACP UsageUpdate is the only enabled source for this family.
    StandardAcpOnly,
    /// A provider-specific adapter slot exists, but no private schema is trusted yet.
    Reserved,
    /// Provider-specific usage is intentionally excluded from the current product scope.
    OutOfScope,
}

#[derive(Debug, Clone, Copy)]
pub enum ProviderUsageInput<'a> {
    SessionUpdateMeta(&'a serde_json::Value),
    PromptResponseMeta(&'a serde_json::Value),
    ExtensionNotification {
        method: &'a str,
        params: &'a serde_json::Value,
    },
    /// A response already obtained by a separately reviewed auth/network source.
    /// Provider adapters parse it; they never read CLI credentials or perform HTTP here.
    ProviderApiResponse {
        schema_id: &'a str,
        body: &'a serde_json::Value,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderUsageRequest<'a> {
    pub reporter_id: Option<&'a str>,
    pub input: ProviderUsageInput<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderContextUsage {
    pub used: u64,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderUsageMetric {
    pub metric_id: String,
    pub value_decimal_text: String,
    pub limit_decimal_text: Option<String>,
    pub unit_id: String,
}

/// Partial provider contribution merged only after the standard ACP normalizer.
/// Optional fields allow a verified extension to report cost without inventing tokens.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderUsageContribution {
    pub context: Option<ProviderContextUsage>,
    pub cost: Option<UsageCost>,
    pub metrics: Vec<ProviderUsageMetric>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderUsageError {
    pub family_id: &'static str,
    pub schema_id: &'static str,
    pub class: &'static str,
}

impl std::fmt::Display for ProviderUsageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "provider usage rejected: family={} schema={} class={}",
            self.family_id, self.schema_id, self.class
        )
    }
}

impl std::error::Error for ProviderUsageError {}

/// Provider-private usage parser. The caller always applies the standard ACP normalizer first,
/// then selects one of these adapters by effective family and verified reporter identity.
pub trait ProviderUsageAdapter: Sync {
    fn family_id(&self) -> &'static str;
    fn private_usage_policy(&self) -> PrivateUsagePolicy;
    fn trusted_reporter_ids(&self) -> &'static [&'static str];
    fn extract_private_usage(
        &self,
        request: ProviderUsageRequest<'_>,
    ) -> Result<ProviderUsageContribution, ProviderUsageError>;
}

static PROVIDERS: [&dyn ProviderUsageAdapter; 5] = [
    &copilot::ADAPTER,
    &claude::ADAPTER,
    &codex::ADAPTER,
    &gemini::ADAPTER,
    &opencode::ADAPTER,
];

pub fn all() -> &'static [&'static dyn ProviderUsageAdapter] {
    &PROVIDERS
}

pub fn lookup(family_id: &str) -> Option<&'static dyn ProviderUsageAdapter> {
    PROVIDERS
        .iter()
        .copied()
        .find(|provider| provider.family_id() == family_id)
}

pub(super) fn no_verified_private_usage(
    request: ProviderUsageRequest<'_>,
) -> Result<ProviderUsageContribution, ProviderUsageError> {
    let _ = request.reporter_id;
    match request.input {
        ProviderUsageInput::SessionUpdateMeta(meta)
        | ProviderUsageInput::PromptResponseMeta(meta) => {
            let _ = meta;
        }
        ProviderUsageInput::ExtensionNotification { method, params } => {
            let _ = (method, params);
        }
        ProviderUsageInput::ProviderApiResponse { schema_id, body } => {
            let _ = (schema_id, body);
        }
    }
    Ok(ProviderUsageContribution::default())
}
