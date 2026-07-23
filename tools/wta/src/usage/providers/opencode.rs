use super::{
    PrivateUsagePolicy, ProviderUsageAdapter, ProviderUsageContribution, ProviderUsageError,
    ProviderUsageRequest,
};

pub(super) struct OpenCodeUsageAdapter;

pub(super) static ADAPTER: OpenCodeUsageAdapter = OpenCodeUsageAdapter;

impl ProviderUsageAdapter for OpenCodeUsageAdapter {
    fn family_id(&self) -> &'static str {
        crate::agent_registry::OPENCODE_AGENT_ID
    }

    fn private_usage_policy(&self) -> PrivateUsagePolicy {
        PrivateUsagePolicy::StandardAcpOnly
    }

    fn trusted_reporter_ids(&self) -> &'static [&'static str] {
        &[]
    }

    fn extract_private_usage(
        &self,
        request: ProviderUsageRequest<'_>,
    ) -> Result<ProviderUsageContribution, ProviderUsageError> {
        super::no_verified_private_usage(request)
    }
}
