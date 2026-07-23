use super::{
    PrivateUsagePolicy, ProviderUsageAdapter, ProviderUsageContribution, ProviderUsageError,
    ProviderUsageRequest,
};

pub(super) struct GeminiUsageAdapter;

pub(super) static ADAPTER: GeminiUsageAdapter = GeminiUsageAdapter;

impl ProviderUsageAdapter for GeminiUsageAdapter {
    fn family_id(&self) -> &'static str {
        crate::agent_registry::GEMINI_AGENT_ID
    }

    fn private_usage_policy(&self) -> PrivateUsagePolicy {
        PrivateUsagePolicy::OutOfScope
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
