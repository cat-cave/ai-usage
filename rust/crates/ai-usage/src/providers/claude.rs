//! Claude provider. See `SPEC.md §2`.
//!
//! Live: `GET https://api.anthropic.com/api/oauth/usage` with the OAuth
//! `accessToken` from `~/.claude/.credentials.json` (`CLAUDE_CONFIG_DIR` honored),
//! plus `anthropic-beta: oauth-2025-04-20`. Velocity from
//! `~/.claude/projects/**/*.jsonl` (`type:"assistant"` + `message.usage`).

use std::path::PathBuf;

use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::model::{
    AccountIdentity, LocalVelocity, Money, PaidOverflow, ProviderId, ProviderReport,
    ProviderStatus, WindowKind, WindowQuota,
};
use crate::provider::Provider;
use crate::source::{Source, Sourced, UnavailableReason};

const USAGE_ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
const ENDPOINT: &str = "claude.oauth-usage";
const VELOCITY_WINDOW_HOURS: u64 = 24;
const BETA: &str = "oauth-2025-04-20";

pub struct ClaudeProvider {
    token: Option<String>,
    client: reqwest::Client,
    config_dir: PathBuf,
}

impl ClaudeProvider {
    pub fn from_config(_cfg: &crate::config::Config) -> Result<Self> {
        let config_dir = std::env::var("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".claude")))
            .map_err(|_| Error::Config("HOME and CLAUDE_CONFIG_DIR both unset".into()))?;
        let cred_path = config_dir.join(".credentials.json");
        let token = if cred_path.exists() {
            let raw = std::fs::read_to_string(&cred_path)?;
            let creds: CredFile = serde_json::from_str(&raw)
                .map_err(|e| Error::Parse(format!("claude credentials: {e}")))?;
            creds.claude_ai_oauth.map(|o| o.access_token)
        } else {
            None
        };
        Ok(Self {
            token,
            client: reqwest::Client::builder()
                .user_agent("ai-usage")
                .build()
                .map_err(|e| Error::Http(format!("client build: {e}")))?,
            config_dir,
        })
    }
}

#[derive(Debug, Deserialize)]
struct CredFile {
    #[serde(default, rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ClaudeOauth>,
}
#[derive(Debug, Deserialize)]
struct ClaudeOauth {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(default, rename = "subscriptionType")]
    subscription_type: Option<String>,
    #[serde(default, rename = "rateLimitTier")]
    rate_limit_tier: Option<String>,
}

#[async_trait]
impl Provider for ClaudeProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Claude
    }
    fn configured(&self) -> bool {
        self.token.is_some()
    }

    async fn fetch(&self) -> Result<ProviderReport> {
        let now = time::OffsetDateTime::now_utc();
        let token = match &self.token {
            Some(t) => t.clone(),
            None => return Ok(Self::unavailable(UnavailableReason::NoCredentials, now)),
        };
        let resp = match self
            .client
            .get(USAGE_ENDPOINT)
            .bearer_auth(&token)
            .header("anthropic-beta", BETA)
            .header("Accept", "application/json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(Self::error_report(
                    UnavailableReason::Network,
                    &format!(
                        "oauth/usage: {}",
                        crate::redact::Redactor::redact_str(&e.to_string())
                    ),
                    now,
                ));
            }
        };
        let status = resp.status().as_u16();
        if status == 401 || status == 403 {
            return Ok(Self::error_report(
                UnavailableReason::AuthFailed,
                &format!("oauth/usage status {status} (re-run claude login)"),
                now,
            ));
        }
        if !resp.status().is_success() {
            return Ok(Self::error_report(
                UnavailableReason::UnknownEndpoint,
                &format!("oauth/usage status {status}"),
                now,
            ));
        }
        let body: UsageResponse = match resp.json().await {
            Ok(b) => b,
            Err(e) => {
                return Ok(Self::error_report(
                    UnavailableReason::Parse,
                    &format!("oauth/usage parse: {e}"),
                    now,
                ));
            }
        };
        Ok(self.build(body, status, now))
    }
}

#[derive(Debug, Default, Deserialize)]
struct UsageResponse {
    #[serde(default)]
    five_hour: Option<UsageWindow>,
    #[serde(default)]
    seven_day: Option<UsageWindow>,
    #[serde(default)]
    seven_day_opus: Option<UsageWindow>,
    #[serde(default)]
    seven_day_sonnet: Option<UsageWindow>,
    #[serde(default)]
    extra_usage: Option<ExtraUsage>,
}

#[derive(Debug, Deserialize)]
struct UsageWindow {
    utilization: f64,
    #[serde(default)]
    resets_at: Option<String>,
}
#[derive(Debug, Default, Deserialize)]
#[allow(dead_code)]
struct ExtraUsage {
    #[serde(default)]
    is_enabled: bool,
    #[serde(default)]
    used_credits: Option<f64>,
    #[serde(default)]
    monthly_limit: Option<f64>,
    #[serde(default)]
    disabled_reason: Option<String>,
}

impl ClaudeProvider {
    fn build(&self, body: UsageResponse, status: u16, now: time::OffsetDateTime) -> ProviderReport {
        let ttl = Some(time::Duration::seconds(60));
        let mut windows = Vec::new();
        push(
            &mut windows,
            body.five_hour.as_ref(),
            WindowKind::Session5h,
            None,
            status,
            now,
        );
        push(
            &mut windows,
            body.seven_day.as_ref(),
            WindowKind::Weekly,
            None,
            status,
            now,
        );
        push(
            &mut windows,
            body.seven_day_opus.as_ref(),
            WindowKind::ModelFamily,
            Some("opus".into()),
            status,
            now,
        );
        push(
            &mut windows,
            body.seven_day_sonnet.as_ref(),
            WindowKind::ModelFamily,
            Some("sonnet".into()),
            status,
            now,
        );

        let paid_overflow = body.extra_usage.map(|eu| {
            let spent = eu
                .used_credits
                .filter(|x| *x > 0.0)
                .map(|v| Money::usd(Decimal::from_f64_retain(v).unwrap_or(Decimal::ZERO)));
            Sourced {
                value: Some(PaidOverflow {
                    enabled: eu.is_enabled,
                    balance: None,
                    spent_this_cycle: spent,
                }),
                source: Source::live(ENDPOINT, Some(status)),
                observed_at: now,
                ttl,
            }
        });

        // Plan label: prefer subscriptionType, fall back to rateLimitTier.
        let (plan, token_src) = (self.subscription_or_tier(), self.token.is_some());
        let _ = token_src;
        let account = Sourced {
            value: Some(AccountIdentity {
                email_masked: None,
                plan,
            }),
            source: Source::live(ENDPOINT, Some(status)),
            observed_at: now,
            ttl,
        };

        let local_velocity = self.local_velocity(now);

        ProviderReport {
            id: ProviderId::Claude,
            account,
            windows,
            banked_resets: vec![],
            plan_capacity: crate::source::unavailable_field(UnavailableReason::NotConfigured),
            paid_overflow: paid_overflow.unwrap_or_else(|| {
                crate::source::unavailable_field(UnavailableReason::NotConfigured)
            }),
            api_balance: crate::source::unavailable_field(UnavailableReason::NotConfigured),
            local_velocity,
            status: ProviderStatus::Ok,
        }
    }

    /// Read the stored plan label from the credentials file (already loaded at
    /// construction; re-read minimally to avoid stashing the token).
    fn subscription_or_tier(&self) -> Option<String> {
        let p = self.config_dir.join(".credentials.json");
        let raw = std::fs::read_to_string(&p).ok()?;
        let f: CredFile = serde_json::from_str(&raw).ok()?;
        let oa = f.claude_ai_oauth?;
        oa.subscription_type.or(oa.rate_limit_tier)
    }

    fn local_velocity(&self, now: time::OffsetDateTime) -> Sourced<Option<LocalVelocity>> {
        let projects = self.config_dir.join("projects");
        if !projects.exists() {
            return crate::source::unavailable_field(UnavailableReason::NotConfigured);
        }
        let cutoff = now - time::Duration::hours(VELOCITY_WINDOW_HOURS as i64);
        let cutoff_st = std::time::SystemTime::from(cutoff);

        let mut total_input: u64 = 0;
        let mut total_output: u64 = 0;
        let mut samples: u32 = 0;
        crate::walk::recent_jsonl(&projects, Some(cutoff_st), 400, &mut |p| {
            scan_usage(p, &mut total_input, &mut total_output, &mut samples);
        });

        if samples == 0 {
            return crate::source::unavailable_field(UnavailableReason::NotConfigured);
        }
        let burn = (total_input + total_output) as f64 / VELOCITY_WINDOW_HOURS as f64;
        Sourced {
            value: Some(LocalVelocity {
                burn_rate_tokens_per_hour: burn,
                window_hours: VELOCITY_WINDOW_HOURS,
                cost_per_hour: None,
                samples,
            }),
            source: Source::LocalLog { path: projects },
            observed_at: now,
            ttl: Some(time::Duration::seconds(30)),
        }
    }

    fn unavailable(reason: UnavailableReason, now: time::OffsetDateTime) -> ProviderReport {
        use crate::source::unavailable_field as u;
        let _ = now;
        ProviderReport {
            id: ProviderId::Claude,
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

    fn error_report(
        reason: UnavailableReason,
        msg: &str,
        now: time::OffsetDateTime,
    ) -> ProviderReport {
        let mut r = Self::unavailable(reason, now);
        r.status = ProviderStatus::Unavailable {
            reason: crate::redact::Redactor::redact_str(msg),
        };
        r
    }
}

fn push(
    out: &mut Vec<Sourced<WindowQuota>>,
    w: Option<&UsageWindow>,
    kind: WindowKind,
    label: Option<String>,
    status: u16,
    now: time::OffsetDateTime,
) {
    if let Some(w) = w {
        let reset_at = w.resets_at.as_deref().and_then(|s| {
            time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
        });
        out.push(Sourced {
            value: WindowQuota {
                kind,
                label,
                used: (w.utilization / 100.0).clamp(0.0, 1.0),
                limit: None,
                used_count: None,
                reset_at,
                rolling: matches!(kind, WindowKind::Session5h),
            },
            source: Source::live(ENDPOINT, Some(status)),
            observed_at: now,
            ttl: Some(time::Duration::seconds(60)),
        });
    }
}

/// Parse a Claude transcript jsonl. Dedup streaming chunks by `message.id` +
/// `requestId`; usage is cumulative per chunk, so keep the max per id.
fn scan_usage(
    path: &std::path::Path,
    total_input: &mut u64,
    total_output: &mut u64,
    samples: &mut u32,
) {
    use std::collections::HashMap;
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let mut seen: HashMap<String, (u64, u64)> = HashMap::new();
    for line in raw.lines() {
        if !line.contains("\"usage\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let Some(usage) = v.get("message").and_then(|m| m.get("usage")) else {
            continue;
        };
        let mid = v
            .get("message")
            .and_then(|m| m.get("id"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let req = v
            .get("requestId")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let key = format!("{mid}::{req}");
        // Active burn only: exclude cache_read_input_tokens (replayed context).
        let input = usage
            .get("input_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let output = usage
            .get("output_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let e = seen.entry(key).or_insert((0, 0));
        if input > e.0 {
            e.0 = input;
        }
        if output > e.1 {
            e.1 = output;
        }
    }
    for (i, o) in seen.values() {
        *total_input += i;
        *total_output += o;
        *samples += 1;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)] // tests assert exact whole-number utilization values
    use super::*;

    #[test]
    fn parses_claude_oauth_usage_live_shape() {
        let raw = serde_json::json!({
            "five_hour": { "utilization": 14, "resets_at": "2026-07-08T18:59:59.705454+00:00" },
            "seven_day": { "utilization": 90, "resets_at": "2026-07-12T11:59:59.705476+00:00" },
            "seven_day_opus": null,
            "seven_day_sonnet": null,
            "extra_usage": { "is_enabled": false, "used_credits": 0, "disabled_reason": "out_of_credits" }
        });
        let body: UsageResponse = serde_json::from_value(raw).unwrap();
        assert_eq!(body.five_hour.unwrap().utilization, 14.0);
        assert_eq!(body.seven_day.unwrap().utilization, 90.0);
        assert!(!body.extra_usage.unwrap().is_enabled);
    }
}
