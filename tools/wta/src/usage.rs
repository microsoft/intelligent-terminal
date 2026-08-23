use agent_client_protocol as acp;

pub mod providers;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageDisplayKind {
    Context,
    Billing,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UsageSnapshot {
    pub context: Option<UsageContext>,
    pub context_display: Option<UsageContextDisplay>,
    pub cost: Option<UsageCost>,
    pub provider_metrics: Vec<UsageProviderMetric>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UsageContext {
    pub used: u64,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UsageContextDisplay {
    pub used_text: String,
    pub size_text: String,
    pub reported_percent: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UsageProviderMetric {
    pub metric_id: String,
    pub display_kind: UsageDisplayKind,
    pub value_decimal_text: String,
    pub limit_decimal_text: Option<String>,
    pub unit_id: String,
    pub unit_display_text: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageStaleness {
    pub context: bool,
    pub cost: bool,
    pub provider_metrics: bool,
}

impl UsageStaleness {
    pub fn mark_reported(&mut self, snapshot: &UsageSnapshot) {
        if snapshot.context.is_some() {
            self.context = false;
        }
        if snapshot.cost.is_some() {
            self.cost = false;
        }
        if !snapshot.provider_metrics.is_empty() {
            self.provider_metrics = false;
        }
    }
}

impl UsageSnapshot {
    pub fn merge(&mut self, incoming: Self) {
        if incoming.context.is_some() {
            self.context = incoming.context;
            self.context_display = incoming.context_display;
        }
        if incoming.cost.is_some() {
            self.cost = incoming.cost;
        }
        for metric in incoming.provider_metrics {
            if let Some(existing) = self
                .provider_metrics
                .iter_mut()
                .find(|existing| existing.metric_id == metric.metric_id)
            {
                *existing = metric;
            }
            else
            {
                self.provider_metrics.push(metric);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UsageCost {
    pub amount_decimal_text: String,
    pub currency: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UsageProjection {
    pub items: Vec<UsageProjectionItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UsageProjectionItem {
    pub metric_id: String,
    pub display_kind: UsageDisplayKind,
    pub value_decimal_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_decimal_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_display_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_display_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reported_percent: Option<u64>,
    pub unit_id: String,
    pub unit_display_text: String,
    pub scope: &'static str,
    pub source: &'static str,
    pub stale: bool,
}

impl From<&UsageSnapshot> for UsageProjection {
    fn from(snapshot: &UsageSnapshot) -> Self {
        Self::with_staleness(snapshot, UsageStaleness::default())
    }
}

impl UsageProjection {
    pub fn with_staleness(snapshot: &UsageSnapshot, staleness: UsageStaleness) -> Self {
        let mut items = Vec::with_capacity(2 + snapshot.provider_metrics.len());
        if let Some(context) = snapshot
            .context
            .as_ref()
            .filter(|context| context.size > 0 && context.used <= context.size)
        {
            items.push(UsageProjectionItem {
                metric_id: "acp.context.window".to_string(),
                display_kind: UsageDisplayKind::Context,
                value_decimal_text: context.used.to_string(),
                limit_decimal_text: Some(context.size.to_string()),
                value_display_text: snapshot.context_display.as_ref().map(|display| display.used_text.clone()),
                limit_display_text: snapshot.context_display.as_ref().map(|display| display.size_text.clone()),
                reported_percent: snapshot.context_display.as_ref().map(|display| display.reported_percent),
                unit_id: "token".to_string(),
                unit_display_text: "token".to_string(),
                scope: "session",
                source: if snapshot.context_display.is_some() { "provider_reported" } else { "acp_standard" },
                stale: staleness.context,
            });
        }
        if let Some(cost) = &snapshot.cost {
            items.push(UsageProjectionItem {
                metric_id: "acp.billing.cost".to_string(),
                display_kind: UsageDisplayKind::Billing,
                value_decimal_text: cost.amount_decimal_text.clone(),
                limit_decimal_text: None,
                value_display_text: None,
                limit_display_text: None,
                reported_percent: None,
                unit_id: cost.currency.clone(),
                unit_display_text: cost.currency.clone(),
                scope: "session",
                source: "acp_standard",
                stale: staleness.cost,
            });
        }
        for metric in &snapshot.provider_metrics {
            items.push(UsageProjectionItem {
                metric_id: metric.metric_id.clone(),
                display_kind: metric.display_kind,
                value_decimal_text: metric.value_decimal_text.clone(),
                limit_decimal_text: metric.limit_decimal_text.clone(),
                value_display_text: None,
                limit_display_text: None,
                reported_percent: None,
                unit_id: metric.unit_id.clone(),
                unit_display_text: metric.unit_display_text.clone(),
                scope: "session",
                source: "provider_reported",
                stale: staleness.provider_metrics,
            });
        }
        Self { items }
    }
}

pub fn normalize_provider_contribution(contribution: providers::ProviderUsageContribution) -> UsageSnapshot {
    let (context, context_display) = contribution.context.map_or((None, None), |context| {
        (
            Some(UsageContext { used: context.used, size: context.size }),
            Some(UsageContextDisplay {
                used_text: context.used_display_text,
                size_text: context.size_display_text,
                reported_percent: context.reported_percent,
            }),
        )
    });
    UsageSnapshot {
        context,
        context_display,
        cost: contribution.cost,
        provider_metrics: contribution.metrics.into_iter().map(|metric| UsageProviderMetric {
            metric_id: metric.metric_id,
            display_kind: metric.display_kind,
            value_decimal_text: metric.value_decimal_text,
            limit_decimal_text: metric.limit_decimal_text,
            unit_id: metric.unit_id,
            unit_display_text: metric.unit_display_text,
        }).collect(),
    }
}

pub fn normalize_standard_usage(update: &acp::schema::v1::UsageUpdate) -> UsageSnapshot {
    let cost = update
        .cost
        .as_ref()
        .filter(|cost| cost.amount.is_finite() && !cost.amount.is_sign_negative())
        .map(|cost| UsageCost {
            amount_decimal_text: cost.amount.to_string(),
            currency: cost.currency.clone(),
        });

    UsageSnapshot {
        context: Some(UsageContext {
            used: update.used,
            size: update.size,
        }),
        context_display: None,
        cost,
        provider_metrics: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol as acp;

    #[test]
    fn normalizes_standard_context_and_cumulative_cost() {
        let update = acp::schema::v1::UsageUpdate::new(1_024, 8_192)
            .cost(acp::schema::v1::Cost::new(0.004, "USD"));

        let snapshot = normalize_standard_usage(&update);

        assert_eq!(
            snapshot.context,
            Some(UsageContext {
                used: 1_024,
                size: 8_192
            })
        );
        assert_eq!(
            snapshot.cost,
            Some(UsageCost {
                amount_decimal_text: "0.004".to_string(),
                currency: "USD".to_string(),
            })
        );
    }

    #[test]
    fn normalizes_standard_context_without_cost() {
        let snapshot = normalize_standard_usage(&acp::schema::v1::UsageUpdate::new(20, 100));

        assert_eq!(
            snapshot.context,
            Some(UsageContext {
                used: 20,
                size: 100
            })
        );
        assert!(snapshot.cost.is_none());
    }

    #[test]
    fn projects_only_context_and_cost_metrics() {
        let snapshot = UsageSnapshot {
            context: Some(UsageContext {
                used: 1_024,
                size: 8_192,
            }),
            context_display: None,
            cost: Some(UsageCost {
                amount_decimal_text: "0.004".to_string(),
                currency: "USD".to_string(),
            }),
            provider_metrics: Vec::new(),
        };

        let projection = UsageProjection::from(&snapshot);

        assert_eq!(projection.items.len(), 2);
        assert_eq!(projection.items[0].metric_id, "acp.context.window");
        assert_eq!(projection.items[1].metric_id, "acp.billing.cost");
        assert_eq!(projection.items[0].display_kind, UsageDisplayKind::Context);
        assert_eq!(projection.items[1].display_kind, UsageDisplayKind::Billing);
        assert_eq!(projection.items[1].unit_display_text, "USD");
    }

    #[test]
    fn merges_independent_context_and_cost_snapshots() {
        let mut snapshot =
            normalize_standard_usage(&acp::schema::v1::UsageUpdate::new(1_024, 8_192));

        snapshot.merge(UsageSnapshot {
            context: None,
            context_display: None,
            cost: Some(UsageCost {
                amount_decimal_text: "0.004".to_string(),
                currency: "USD".to_string(),
            }),
            provider_metrics: Vec::new(),
        });

        assert_eq!(
            snapshot.context,
            Some(UsageContext {
                used: 1_024,
                size: 8_192
            })
        );
        assert_eq!(snapshot.cost.expect("cost").currency, "USD");
    }

    #[test]
    fn normalizes_and_projects_provider_context_and_aic() {
        let snapshot = normalize_provider_contribution(providers::ProviderUsageContribution {
            context: Some(providers::ProviderContextUsage {
                used: 30_000,
                size: 264_000,
                used_display_text: "30k".to_string(),
                size_display_text: "264k".to_string(),
                reported_percent: 11,
            }),
            metrics: vec![providers::ProviderUsageMetric {
                metric_id: "github.copilot.ai_credits".to_string(),
                display_kind: UsageDisplayKind::Billing,
                value_decimal_text: "7.5539".to_string(),
                limit_decimal_text: None,
                unit_id: "AIC".to_string(),
                unit_display_text: "AIC".to_string(),
            }],
            ..Default::default()
        });

        assert!(snapshot.cost.is_none(), "AIC is not monetary cost");
        let projection = UsageProjection::from(&snapshot);
        assert_eq!(projection.items.len(), 2);
        assert_eq!(projection.items[0].metric_id, "acp.context.window");
        assert_eq!(projection.items[0].value_display_text.as_deref(), Some("30k"));
        assert_eq!(projection.items[0].limit_display_text.as_deref(), Some("264k"));
        assert_eq!(projection.items[0].reported_percent, Some(11));
        assert_eq!(projection.items[0].source, "provider_reported");
        assert_eq!(projection.items[1].metric_id, "github.copilot.ai_credits");
        assert_eq!(projection.items[1].value_decimal_text, "7.5539");
        assert_eq!(projection.items[1].unit_id, "AIC");
        assert_eq!(projection.items[1].display_kind, UsageDisplayKind::Billing);
        assert_eq!(projection.items[1].unit_display_text, "AIC");
        assert_eq!(projection.items[1].source, "provider_reported");
    }

    #[test]
    fn hides_invalid_context_projection_without_hiding_billing() {
        for (used, size) in [(1, 0), (101, 100)] {
            let snapshot = UsageSnapshot {
                context: Some(UsageContext { used, size }),
                context_display: None,
                cost: Some(UsageCost {
                    amount_decimal_text: "0.004".to_string(),
                    currency: "USD".to_string(),
                }),
                provider_metrics: Vec::new(),
            };

            let projection = UsageProjection::from(&snapshot);
            assert_eq!(projection.items.len(), 1);
            assert_eq!(projection.items[0].display_kind, UsageDisplayKind::Billing);
        }

        let valid = UsageProjection::from(&UsageSnapshot {
            context: Some(UsageContext { used: 100, size: 100 }),
            context_display: None,
            cost: None,
            provider_metrics: Vec::new(),
        });
        assert_eq!(valid.items.len(), 1, "a full but valid context remains useful");
        assert_eq!(valid.items[0].display_kind, UsageDisplayKind::Context);
    }

    #[test]
    fn omits_non_finite_or_negative_cost_without_discarding_context() {
        for amount in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.01] {
            let update = acp::schema::v1::UsageUpdate::new(1, 100)
                .cost(acp::schema::v1::Cost::new(amount, "USD"));
            let snapshot = normalize_standard_usage(&update);

            assert_eq!(snapshot.context, Some(UsageContext { used: 1, size: 100 }));
            assert!(snapshot.cost.is_none());
        }
    }

    #[test]
    fn preserves_provider_reported_currency_without_shape_policy() {
        for currency in ["EU", "EURO", "lowercase", "EU1", "E$U"] {
            let update = acp::schema::v1::UsageUpdate::new(1, 100)
                .cost(acp::schema::v1::Cost::new(1.0, currency));
            let snapshot = normalize_standard_usage(&update);

            assert_eq!(snapshot.context, Some(UsageContext { used: 1, size: 100 }));
            assert_eq!(
                snapshot.cost,
                Some(UsageCost {
                    amount_decimal_text: "1".to_string(),
                    currency: currency.to_string(),
                })
            );
        }
    }

    #[test]
    fn provider_registry_covers_every_known_agent_family() {
        let mut registered = providers::all()
            .iter()
            .map(|provider| provider.family_id())
            .collect::<Vec<_>>();
        registered.sort_unstable();

        let mut known = crate::agent_registry::KNOWN_AGENTS
            .iter()
            .map(|profile| profile.id)
            .collect::<Vec<_>>();
        known.sort_unstable();

        assert_eq!(registered, known);
    }

    #[test]
    fn provider_registry_declares_current_private_usage_policy() {
        use providers::PrivateUsagePolicy;

        assert_eq!(
            providers::lookup("copilot").unwrap().private_usage_policy(),
            PrivateUsagePolicy::StandardAcpOnly
        );
        assert_eq!(
            providers::lookup("claude").unwrap().private_usage_policy(),
            PrivateUsagePolicy::StandardAcpOnly
        );
        assert_eq!(
            providers::lookup("codex").unwrap().private_usage_policy(),
            PrivateUsagePolicy::StandardAcpOnly
        );
        assert_eq!(
            providers::lookup("gemini").unwrap().private_usage_policy(),
            PrivateUsagePolicy::StandardAcpOnly
        );
        assert_eq!(
            providers::lookup("opencode")
                .unwrap()
                .private_usage_policy(),
            PrivateUsagePolicy::StandardAcpOnly
        );
    }

    #[test]
    fn provider_adapters_do_not_invent_unverified_private_usage() {
        let meta = serde_json::json!({ "unverified": { "amount": 12345 } });
        let notification = serde_json::json!({ "credits": 98765 });
        let inputs = [
            providers::ProviderUsageInput::SessionUpdateMeta(&meta),
            providers::ProviderUsageInput::PromptResponseMeta(&meta),
            providers::ProviderUsageInput::ExtensionNotification {
                method: "vendor/private-usage",
                params: &notification,
            },
            providers::ProviderUsageInput::ProviderApiResponse {
                schema_id: "vendor.usage.v1",
                body: &notification,
            },
        ];

        for provider in providers::all().iter().copied() {
            if provider.private_usage_policy()
                == providers::PrivateUsagePolicy::VerifiedCommandProbe
            {
                continue;
            }
            assert!(
                provider.trusted_reporter_ids().is_empty(),
                "{} must not trust a private reporter before wire verification",
                provider.family_id()
            );
            for reporter_id in [None, Some("impostor-reporter")] {
                for input in &inputs {
                    assert_eq!(
                        provider
                            .extract_private_usage(providers::ProviderUsageRequest {
                                reporter_id,
                                input: *input,
                            })
                            .unwrap(),
                        providers::ProviderUsageContribution::default(),
                        "{} must stay no-op until its schema is verified",
                        provider.family_id()
                    );
                }
            }
        }
    }

    #[test]
    fn unknown_or_custom_agents_have_no_private_provider_adapter() {
        assert!(providers::lookup("unknown").is_none());
        assert!(providers::lookup("custom:npx").is_none());
    }
}
