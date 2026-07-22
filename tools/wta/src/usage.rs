use agent_client_protocol as acp;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UsageSnapshot {
    pub used: u64,
    pub size: u64,
    pub cost: Option<UsageCost>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UsageCost {
    pub amount_decimal_text: String,
    pub currency: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageError {
    ZeroContextSize,
    ContextUsedExceedsSize { used: u64, size: u64 },
    InvalidCostAmount,
    InvalidCurrency,
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroContextSize => formatter.write_str("usage context size must be non-zero"),
            Self::ContextUsedExceedsSize { used, size } => {
                write!(
                    formatter,
                    "usage context used ({used}) exceeds size ({size})"
                )
            }
            Self::InvalidCostAmount => {
                formatter.write_str("usage cost amount must be finite and non-negative")
            }
            Self::InvalidCurrency => {
                formatter.write_str("usage currency must be three uppercase ASCII letters")
            }
        }
    }
}

impl std::error::Error for UsageError {}

pub fn normalize_standard_usage(
    update: &acp::schema::v1::UsageUpdate,
) -> Result<UsageSnapshot, UsageError> {
    if update.size == 0 {
        return Err(UsageError::ZeroContextSize);
    }
    if update.used > update.size {
        return Err(UsageError::ContextUsedExceedsSize {
            used: update.used,
            size: update.size,
        });
    }

    let cost = update
        .cost
        .as_ref()
        .map(|cost| {
            if !cost.amount.is_finite() || cost.amount.is_sign_negative() {
                return Err(UsageError::InvalidCostAmount);
            }
            if cost.currency.len() != 3
                || !cost.currency.bytes().all(|byte| byte.is_ascii_uppercase())
            {
                return Err(UsageError::InvalidCurrency);
            }
            Ok(UsageCost {
                amount_decimal_text: cost.amount.to_string(),
                currency: cost.currency.clone(),
            })
        })
        .transpose()?;

    Ok(UsageSnapshot {
        used: update.used,
        size: update.size,
        cost,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol as acp;

    #[test]
    fn normalizes_standard_context_and_cumulative_cost() {
        let update = acp::schema::v1::UsageUpdate::new(1_024, 8_192)
            .cost(acp::schema::v1::Cost::new(0.004, "USD"));

        let snapshot = normalize_standard_usage(&update).expect("valid standard usage");

        assert_eq!(snapshot.used, 1_024);
        assert_eq!(snapshot.size, 8_192);
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
        let snapshot = normalize_standard_usage(&acp::schema::v1::UsageUpdate::new(20, 100))
            .expect("cost is optional");

        assert_eq!(snapshot.used, 20);
        assert_eq!(snapshot.size, 100);
        assert!(snapshot.cost.is_none());
    }

    #[test]
    fn rejects_invalid_context_ratio() {
        assert_eq!(
            normalize_standard_usage(&acp::schema::v1::UsageUpdate::new(1, 0)),
            Err(UsageError::ZeroContextSize)
        );
        assert_eq!(
            normalize_standard_usage(&acp::schema::v1::UsageUpdate::new(101, 100)),
            Err(UsageError::ContextUsedExceedsSize {
                used: 101,
                size: 100,
            })
        );
    }

    #[test]
    fn rejects_non_finite_or_negative_cost() {
        for amount in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.01] {
            let update = acp::schema::v1::UsageUpdate::new(1, 100)
                .cost(acp::schema::v1::Cost::new(amount, "USD"));
            assert_eq!(
                normalize_standard_usage(&update),
                Err(UsageError::InvalidCostAmount)
            );
        }
    }

    #[test]
    fn rejects_non_canonical_currency_shape() {
        for currency in ["US", "USDD", "usd", "US1", "U$D"] {
            let update = acp::schema::v1::UsageUpdate::new(1, 100)
                .cost(acp::schema::v1::Cost::new(1.0, currency));
            assert_eq!(
                normalize_standard_usage(&update),
                Err(UsageError::InvalidCurrency)
            );
        }
    }
}
