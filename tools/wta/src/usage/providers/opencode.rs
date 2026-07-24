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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{normalize_standard_usage, UsageCost, UsageSnapshot};
    use agent_client_protocol as acp;

    #[test]
    fn normalizes_verified_opencode_1_18_3_standard_usage_capture() {
        let update: acp::schema::v1::UsageUpdate = serde_json::from_str(
            r#"{"used":6092,"size":271790,"cost":{"amount":0,"currency":"USD"}}"#,
        )
        .expect("verified OpenCode UsageUpdate should deserialize");

        assert_eq!(
            normalize_standard_usage(&update),
            Ok(UsageSnapshot {
                used: 6092,
                size: 271790,
                cost: Some(UsageCost {
                    amount_decimal_text: "0".to_string(),
                    currency: "USD".to_string(),
                }),
            })
        );
        assert_eq!(
            ADAPTER.private_usage_policy(),
            PrivateUsagePolicy::StandardAcpOnly
        );
        assert!(ADAPTER.trusted_reporter_ids().is_empty());
    }
}
