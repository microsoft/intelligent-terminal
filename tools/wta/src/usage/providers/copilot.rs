use super::{
    PrivateUsagePolicy, ProviderUsageAdapter, ProviderUsageContribution, ProviderUsageError,
    ProviderUsageRequest,
};

pub(super) struct CopilotUsageAdapter;

pub(super) static ADAPTER: CopilotUsageAdapter = CopilotUsageAdapter;

impl ProviderUsageAdapter for CopilotUsageAdapter {
    fn family_id(&self) -> &'static str {
        crate::agent_registry::COPILOT_AGENT_ID
    }

    fn private_usage_policy(&self) -> PrivateUsagePolicy {
        PrivateUsagePolicy::Reserved
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
