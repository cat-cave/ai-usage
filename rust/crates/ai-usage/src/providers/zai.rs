//! z.ai provider. See `SPEC.md §2`.
//!
//! `GET https://api.z.ai/api/monitor/usage/quota/limit` (Bearer). BigModel CN
//! host override via `Z_AI_API_HOST=open.bigmodel.cn`. Team scope adds
//! `type=2` + `Bigmodel-Organization`/`Bigmodel-Project` headers (not in v1
//! personal scope). Secret file: `zai-api-key`.
//!
//! Response (per CodexBar docs): `data.limits[]` each with type/unit/number and
//! `nextResetTime`; `data.planName`. `TOKENS_LIMIT` → primary tokens window;
//! `TIME_LIMIT` → MCP/time window.

use async_trait::async_trait;
use serde::Deserialize;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::model::{
    AccountIdentity, ProviderId, ProviderReport, ProviderStatus, WindowKind, WindowQuota,
};
use crate::provider::Provider;
use crate::source::{Source, Sourced, UnavailableReason};

const DEFAULT_HOST: &str = "api.z.ai";
const PATH: &str = "/api/monitor/usage/quota/limit";
const ENDPOINT: &str = "zai.quota-limit";

pub struct ZaiProvider {
    token: Option<String>,
    host: String,
    client: reqwest::Client,
}

impl ZaiProvider {
    pub fn from_config(cfg: &Config) -> Result<Self> {
        let token = cfg.resolve_provider_key(
            "zai",
            "zai-coding-plan",
            cfg.providers
                .zai
                .as_ref()
                .and_then(|p| p.api_key_path.as_deref()),
        )?;
        let host = std::env::var("Z_AI_API_HOST")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                crate::config::assert_https(&s)?;
                Ok::<String, Error>(s.trim_end_matches('/').to_string())
            })
            .transpose()?
            .unwrap_or_else(|| DEFAULT_HOST.to_string());
        Ok(Self {
            token,
            host,
            client: reqwest::Client::builder()
                .user_agent("ai-usage")
                .build()
                .map_err(|e| Error::Http(format!("client build: {e}")))?,
        })
    }
}

#[async_trait]
impl Provider for ZaiProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Zai
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
        let url = format!("https://{}{}", self.host, PATH);
        let resp = match self
            .client
            .get(&url)
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
                        "quota/limit: {}",
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
        let body: QuotaResponse = match resp.json().await {
            Ok(b) => b,
            Err(e) => return Ok(error_report(UnavailableReason::Parse, &format!("{e}"), now)),
        };
        Ok(build(&body, status, now))
    }
}

#[derive(Debug, Default, Deserialize)]
struct QuotaResponse {
    #[serde(default)]
    data: Option<QuotaData>,
}
#[derive(Debug, Default, Deserialize)]
struct QuotaData {
    #[serde(default)]
    limits: Vec<Limit>,
    /// z.ai uses `level` for the plan label (e.g. "max"); accept `planName`/`plan` too.
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    plan_name: Option<String>,
    #[serde(default, rename = "plan")]
    plan_alt: Option<String>,
}
#[derive(Debug, Deserialize)]
#[allow(dead_code, clippy::struct_field_names)] // `limit_type` holds the upstream `type` field (Rust keyword)
struct Limit {
    #[serde(rename = "type")]
    limit_type: Option<String>,
    /// Window unit code (integer). Was wrongly typed as String, which broke
    /// decoding against the real response.
    #[serde(default)]
    unit: Option<u64>,
    #[serde(default)]
    number: Option<u64>,
    #[serde(default)]
    usage: Option<u64>,
    #[serde(default, rename = "currentValue")]
    current_value: Option<u64>,
    #[serde(default)]
    remaining: Option<u64>,
    /// Utilization in percent (0–100). This is the canonical "used" metric.
    #[serde(default)]
    percentage: Option<f64>,
    #[serde(default, rename = "nextResetTime")]
    next_reset_time: Option<u64>,
}

fn build(body: &QuotaResponse, status: u16, now: time::OffsetDateTime) -> ProviderReport {
    use crate::source::unavailable_field;
    let ttl = Some(time::Duration::seconds(60));

    let Some(data) = body.data.as_ref() else {
        return unavailable(UnavailableReason::Parse, now);
    };

    let mut windows = Vec::new();
    for lim in &data.limits {
        let kind = match lim.limit_type.as_deref() {
            Some("TOKENS_LIMIT") => WindowKind::Coding,
            Some("TIME_LIMIT") => WindowKind::Mcp,
            _ => continue,
        };
        // `percentage` is the canonical utilization (0–100).
        let used = lim.percentage.map_or(0.0, |p| p / 100.0);
        let reset_at = lim.next_reset_time.and_then(|ms| {
            time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok()
        });
        windows.push(Sourced {
            value: WindowQuota {
                kind,
                label: lim.limit_type.clone(),
                used: used.clamp(0.0, 1.0),
                limit: lim.usage,
                used_count: lim.current_value,
                reset_at,
                rolling: false,
            },
            source: Source::live(ENDPOINT, Some(status)),
            observed_at: now,
            ttl,
        });
    }

    let account = Sourced {
        value: Some(AccountIdentity {
            email_masked: None,
            plan: data
                .level
                .clone()
                .or_else(|| data.plan_name.clone())
                .or_else(|| data.plan_alt.clone()),
        }),
        source: Source::live(ENDPOINT, Some(status)),
        observed_at: now,
        ttl,
    };

    ProviderReport {
        id: ProviderId::Zai,
        account,
        windows,
        banked_resets: vec![],
        plan_capacity: unavailable_field(UnavailableReason::NotConfigured),
        paid_overflow: unavailable_field(UnavailableReason::NotConfigured),
        api_balance: unavailable_field(UnavailableReason::NotConfigured),
        local_velocity: unavailable_field(UnavailableReason::NotConfigured),
        status: ProviderStatus::Ok,
    }
}

fn unavailable(reason: UnavailableReason, now: time::OffsetDateTime) -> ProviderReport {
    use crate::source::unavailable_field;
    let _ = now;
    ProviderReport {
        id: ProviderId::Zai,
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
