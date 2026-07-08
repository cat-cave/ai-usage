//! OpenRouter provider — full reference implementation. See `SPEC.md §2`.
//!
//! - `GET /api/v1/credits` → `total_credits`, `total_usage`
//! - `GET /api/v1/key`     → `limit`, `usage`, `limit_remaining`
//!
//! Auth: `OPENROUTER_API_KEY` / `_FILE` / config. HTTPS-only URL override.

use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::model::{
    ApiBalance, Money, PaidOverflow, ProviderId, ProviderReport, ProviderStatus, WindowKind,
    WindowQuota,
};
use crate::provider::Provider;
use crate::source::{Source, Sourced, UnavailableReason};

const DEFAULT_BASE: &str = "https://openrouter.ai/api/v1";
const ENDPOINT_CREDITS: &str = "openrouter.credits";
const ENDPOINT_KEY: &str = "openrouter.key";

pub struct OpenRouterProvider {
    token: Option<String>,
    base: String,
    client: reqwest::Client,
}

impl OpenRouterProvider {
    pub fn from_config(cfg: &Config) -> Result<Self> {
        let token = cfg.resolve_provider_key(
            "openrouter",
            "openrouter",
            cfg.providers
                .openrouter
                .as_ref()
                .and_then(|p| p.api_key_path.as_deref()),
        )?;

        let base = match std::env::var("OPENROUTER_API_URL") {
            Ok(u) if !u.trim().is_empty() => {
                crate::config::assert_https(&u)?;
                u.trim_end_matches('/').to_string()
            }
            _ => DEFAULT_BASE.to_string(),
        };

        // Suffix the base with /api/v1 only if the user gave a bare host.
        let base = if base.contains("/api/v1") {
            base
        } else {
            format!("{}/api/v1", base.trim_end_matches('/'))
        };

        Ok(Self {
            token,
            base,
            client: reqwest::Client::builder()
                .user_agent("ai-usage")
                .build()
                .map_err(|e| Error::Http(format!("client build: {e}")))?,
        })
    }

    async fn get(&self, path: &str, endpoint: &'static str) -> Result<reqwest::Response> {
        let url = format!("{}{}", self.base, path);
        let token = self.token.as_deref().ok_or_else(|| {
            Error::Config("openrouter: no API key (OPENROUTER_API_KEY/_FILE/config)".into())
        })?;
        let resp = self
            .client
            .get(&url)
            .bearer_auth(token)
            .header("HTTP-Referer", "https://github.com/cat-cave/ai-usage")
            .header("X-Title", "ai-usage")
            .send()
            .await
            .map_err(|e| {
                Error::Http(format!(
                    "{}: {}",
                    endpoint,
                    crate::redact::Redactor::redact_str(&e.to_string())
                ))
            })?;
        Ok(resp)
    }
}

#[async_trait]
impl Provider for OpenRouterProvider {
    fn id(&self) -> ProviderId {
        ProviderId::OpenRouter
    }

    fn configured(&self) -> bool {
        self.token.is_some()
    }

    async fn fetch(&self) -> Result<ProviderReport> {
        if self.token.is_none() {
            return Ok(unavailable_report(UnavailableReason::NoCredentials));
        }

        let credits_resp = match self.get("/credits", ENDPOINT_CREDITS).await {
            Ok(r) => r,
            Err(e) => return Ok(error_report(UnavailableReason::Network, &e.to_string())),
        };
        let sc = credits_resp.status().as_u16();
        if !credits_resp.status().is_success() {
            return Ok(error_report(
                if sc == 401 || sc == 403 {
                    UnavailableReason::AuthFailed
                } else {
                    UnavailableReason::UnknownEndpoint
                },
                &format!("credits status {sc}"),
            ));
        }
        let credits: CreditsResponse = credits_resp
            .json()
            .await
            .map_err(|e| Error::Parse(format!("credits: {e}")))?;

        let key: Option<KeyResponse> = match self.get("/key", ENDPOINT_KEY).await {
            Ok(r) if r.status().is_success() => r.json().await.ok(),
            _ => None,
        };

        Ok(build_report(&credits, key.as_ref(), sc))
    }
}

#[derive(Debug, Deserialize)]
pub struct CreditsResponse {
    pub data: CreditsData,
}
#[derive(Debug, Deserialize)]
pub struct CreditsData {
    #[serde(default)]
    pub total_credits: f64,
    #[serde(default)]
    pub total_usage: f64,
}

#[derive(Debug, Deserialize)]
pub struct KeyResponse {
    pub data: KeyData,
}
#[derive(Debug, Deserialize, Default)]
pub struct KeyData {
    #[serde(default)]
    pub limit: Option<f64>,
    #[serde(default)]
    pub usage: Option<f64>,
    #[serde(default)]
    pub limit_remaining: Option<f64>,
}

/// Pure: map parsed API responses → report. Unit-tested without network.
pub fn build_report(
    credits: &CreditsResponse,
    key: Option<&KeyResponse>,
    status_code: u16,
) -> ProviderReport {
    use crate::model::Currency;
    use crate::source::unavailable_field as unavail;
    let total_credits = credits.data.total_credits;
    let total_usage = credits.data.total_usage;
    let balance = (total_credits - total_usage).max(0.0);
    let now = time::OffsetDateTime::now_utc();

    let balance_money = Money {
        amount: Decimal::from_f64_retain(balance).unwrap_or(Decimal::ZERO),
        currency: Currency::Usd,
    };
    let paid_money = Money::usd(Decimal::from_f64_retain(total_credits).unwrap_or(Decimal::ZERO));
    let spent_money = Money::usd(Decimal::from_f64_retain(total_usage).unwrap_or(Decimal::ZERO));

    let live_src = || Source::live(ENDPOINT_CREDITS, Some(status_code));

    let api_balance = Sourced {
        value: Some(ApiBalance {
            total: balance_money,
            granted: None,
            paid: Some(paid_money),
        }),
        source: live_src(),
        observed_at: now,
        ttl: Some(time::Duration::seconds(60)),
    };

    let paid_overflow = Sourced {
        value: Some(PaidOverflow {
            enabled: true, // OpenRouter is pay-as-you-go
            balance: Some(balance_money),
            spent_this_cycle: Some(spent_money),
        }),
        source: live_src(),
        observed_at: now,
        ttl: Some(time::Duration::seconds(60)),
    };

    let mut windows = Vec::new();
    if let Some(kd) = key.map(|k| &k.data) {
        if let (Some(limit), Some(usage)) = (kd.limit, kd.usage) {
            if limit > 0.0 {
                let used = (usage / limit).clamp(0.0, 1.0);
                windows.push(Sourced {
                    value: WindowQuota {
                        kind: WindowKind::Monthly,
                        label: Some("api key".into()),
                        used,
                        limit: Some(limit as u64),
                        used_count: Some(usage as u64),
                        reset_at: None,
                        rolling: false,
                    },
                    source: Source::live(ENDPOINT_KEY, Some(status_code)),
                    observed_at: now,
                    ttl: Some(time::Duration::seconds(60)),
                });
            }
        }
    }

    ProviderReport {
        id: ProviderId::OpenRouter,
        account: unavail(UnavailableReason::NotConfigured),
        windows,
        banked_resets: vec![],
        plan_capacity: unavail(UnavailableReason::NotConfigured),
        paid_overflow,
        api_balance,
        local_velocity: unavail(UnavailableReason::NotConfigured),
        status: ProviderStatus::Ok,
    }
}

fn unavailable_report(reason: UnavailableReason) -> ProviderReport {
    use crate::source::unavailable_field as u;
    ProviderReport {
        id: ProviderId::OpenRouter,
        account: u(reason),
        windows: vec![],
        banked_resets: vec![],
        plan_capacity: u(reason),
        paid_overflow: u(reason),
        api_balance: u(reason),
        local_velocity: u(reason),
        status: ProviderStatus::Unavailable {
            reason: format!("{reason:?}").to_lowercase(),
        },
    }
}

fn error_report(reason: UnavailableReason, msg: &str) -> ProviderReport {
    let mut r = unavailable_report(reason);
    r.status = ProviderStatus::Unavailable {
        reason: crate::redact::Redactor::redact_str(msg),
    };
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_credits_and_balance() {
        let credits: CreditsResponse = serde_json::from_value(json!({
            "data": { "total_credits": 50.0, "total_usage": 7.6 }
        }))
        .unwrap();
        let report = build_report(&credits, None, 200);
        let bal = report.api_balance.value.unwrap();
        assert_eq!(bal.total.amount.round_dp(2), Decimal::new(4240, 2)); // 42.40
        assert!(bal.paid.unwrap().amount > Decimal::ZERO);
        assert!(matches!(report.paid_overflow.value, Some(po) if po.enabled));
    }

    #[test]
    fn key_limit_adds_a_window() {
        let credits: CreditsResponse = serde_json::from_value(json!({
            "data": { "total_credits": 100.0, "total_usage": 25.0 }
        }))
        .unwrap();
        let key: KeyResponse = serde_json::from_value(json!({
            "data": { "limit": 100.0, "usage": 40.0, "limit_remaining": 60.0 }
        }))
        .unwrap();
        let report = build_report(&credits, Some(&key), 200);
        assert_eq!(report.windows.len(), 1);
        let w = &report.windows[0].value;
        assert!((w.used - 0.4).abs() < 1e-6);
    }
}
