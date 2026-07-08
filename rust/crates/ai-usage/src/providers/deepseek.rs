//! DeepSeek provider. See `SPEC.md §2`.
//!
//! `GET https://api.deepseek.com/user/balance` (Bearer) → `balance_infos[]` with
//! `total_balance`, `granted_balance`, `topped_up_balance`. Pure-API balance;
//! no windowed quota. Secret file: `deepseek-api-key`.

use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::model::{ApiBalance, Currency, Money, ProviderId, ProviderReport, ProviderStatus};
use crate::provider::Provider;
use crate::source::{Source, Sourced, UnavailableReason};

const DEFAULT_BASE: &str = "https://api.deepseek.com";
const ENDPOINT: &str = "deepseek.balance";

pub struct DeepSeekProvider {
    token: Option<String>,
    base: String,
    client: reqwest::Client,
}

impl DeepSeekProvider {
    pub fn from_config(cfg: &Config) -> Result<Self> {
        let token = cfg.resolve_provider_key(
            "deepseek",
            "deepseek",
            cfg.providers
                .deepseek
                .as_ref()
                .and_then(|p| p.api_key_path.as_deref()),
        )?;
        let base = std::env::var("DEEPSEEK_API_BASE")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                crate::config::assert_https(&s)?;
                Ok::<String, Error>(s.trim_end_matches('/').to_string())
            })
            .transpose()?
            .unwrap_or_else(|| DEFAULT_BASE.to_string());
        Ok(Self {
            token,
            base,
            client: reqwest::Client::builder()
                .user_agent("ai-usage")
                .build()
                .map_err(|e| Error::Http(format!("client build: {e}")))?,
        })
    }
}

#[async_trait]
impl Provider for DeepSeekProvider {
    fn id(&self) -> ProviderId {
        ProviderId::DeepSeek
    }
    fn configured(&self) -> bool {
        self.token.is_some()
    }

    async fn fetch(&self) -> Result<ProviderReport> {
        let now = time::OffsetDateTime::now_utc();
        let token = match &self.token {
            Some(t) => t.clone(),
            None => return Ok(unavailable(UnavailableReason::NoCredentials, now)),
        };
        let resp = match self
            .client
            .get(format!("{}/user/balance", self.base))
            .bearer_auth(token)
            .header("Accept", "application/json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(error_report(
                    UnavailableReason::Network,
                    &format!(
                        "balance: {}",
                        crate::redact::Redactor::redact_str(&e.to_string())
                    ),
                    now,
                ));
            }
        };
        let status = resp.status().as_u16();
        if status == 401 || status == 403 {
            return Ok(error_report(
                UnavailableReason::AuthFailed,
                &format!("status {status}"),
                now,
            ));
        }
        if !resp.status().is_success() {
            return Ok(error_report(
                UnavailableReason::UnknownEndpoint,
                &format!("status {status}"),
                now,
            ));
        }
        let body: BalanceResponse = match resp.json().await {
            Ok(b) => b,
            Err(e) => return Ok(error_report(UnavailableReason::Parse, &format!("{e}"), now)),
        };
        Ok(build(&body, status, now))
    }
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BalanceResponse {
    #[serde(default)]
    is_available: bool,
    #[serde(default)]
    balance_infos: Vec<BalanceInfo>,
}
#[derive(Debug, Deserialize)]
struct BalanceInfo {
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    total_balance: Option<String>,
    #[serde(default)]
    granted_balance: Option<String>,
    #[serde(default)]
    topped_up_balance: Option<String>,
}

fn parse_money(s: Option<&str>, currency: Currency) -> Option<Money> {
    s.and_then(|x| x.parse::<f64>().ok()).map(|v| Money {
        amount: Decimal::from_f64_retain(v).unwrap_or(Decimal::ZERO),
        currency,
    })
}

fn build(body: &BalanceResponse, status: u16, now: time::OffsetDateTime) -> ProviderReport {
    use crate::source::unavailable_field;
    let ttl = Some(time::Duration::seconds(60));

    // Prefer USD when multiple currencies are present.
    let info = body
        .balance_infos
        .iter()
        .find(|i| i.currency.as_deref() == Some("USD"))
        .or_else(|| body.balance_infos.first());

    let api_balance = info.map_or_else(
        || unavailable_field(UnavailableReason::Parse),
        |i| {
            let currency = match i.currency.as_deref() {
                Some("USD") => Currency::Usd,
                Some("CNY") => Currency::Cny,
                Some("EUR") => Currency::Eur,
                _ => Currency::Unknown,
            };
            Sourced {
                value: Some(ApiBalance {
                    total: parse_money(i.total_balance.as_deref(), currency).unwrap_or(Money {
                        amount: Decimal::ZERO,
                        currency,
                    }),
                    granted: parse_money(i.granted_balance.as_deref(), currency),
                    paid: parse_money(i.topped_up_balance.as_deref(), currency),
                }),
                source: Source::live(ENDPOINT, Some(status)),
                observed_at: now,
                ttl,
            }
        },
    );

    ProviderReport {
        id: ProviderId::DeepSeek,
        account: unavailable_field(UnavailableReason::NotConfigured),
        windows: vec![],
        banked_resets: vec![],
        plan_capacity: unavailable_field(UnavailableReason::NotConfigured),
        paid_overflow: unavailable_field(UnavailableReason::NotConfigured),
        api_balance,
        local_velocity: unavailable_field(UnavailableReason::NotConfigured),
        status: if info.is_some() {
            ProviderStatus::Ok
        } else {
            ProviderStatus::Degraded {
                notes: vec!["no balance infos".into()],
            }
        },
    }
}

fn unavailable(reason: UnavailableReason, now: time::OffsetDateTime) -> ProviderReport {
    use crate::source::unavailable_field;
    let _ = now;
    ProviderReport {
        id: ProviderId::DeepSeek,
        account: unavailable_field(reason),
        windows: vec![],
        banked_resets: vec![],
        plan_capacity: unavailable_field(reason),
        paid_overflow: unavailable_field(reason),
        api_balance: unavailable_field(reason),
        local_velocity: unavailable_field(reason),
        status: ProviderStatus::Unavailable {
            reason: format!("{reason:?}").to_lowercase(),
        },
    }
}

fn error_report(reason: UnavailableReason, msg: &str, now: time::OffsetDateTime) -> ProviderReport {
    let mut r = unavailable(reason, now);
    r.status = ProviderStatus::Unavailable {
        reason: crate::redact::Redactor::redact_str(msg),
    };
    r
}
