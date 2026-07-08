//! Codex provider. See `SPEC.md §2`.
//!
//! Live: `GET https://chatgpt.com/backend-api/wham/usage` with the OAuth
//! `access_token` read from `~/.codex/auth.json` (`CODEX_HOME` honored). Falls
//! back to a local-velocity scan of `~/.codex/sessions/**/*.jsonl`. Token refresh
//! is a follow-up; an expired/expiring token surfaces as `AuthFailed`.
//!
//! Real response shape (discovered live, redacted):
//!   rate_limit.primary_window   { used_percent, limit_window_seconds, reset_after_seconds, reset_at(unix s) }
//!   rate_limit.secondary_window  (same) — weekly
//!   additional_rate_limits[]    { limit_name, metered_feature, rate_limit: {primary_window, secondary_window} }
//!   credits                     { has_credits, unlimited, balance(str) }
//!   rate_limit_reset_credits    { available_count }
//!   plan_type, email

use std::path::PathBuf;
use std::time::SystemTime;

use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::model::{
    AccountIdentity, BankedReset, LocalVelocity, Money, PaidOverflow, ProviderId, ProviderReport,
    ProviderStatus, WindowKind, WindowQuota,
};
use crate::provider::Provider;
use crate::source::{Source, Sourced, UnavailableReason};

const USAGE_ENDPOINT: &str = "https://chatgpt.com/backend-api/wham/usage";
const ENDPOINT: &str = "codex.wham-usage";
const VELOCITY_WINDOW_HOURS: u64 = 24;

pub struct CodexProvider {
    token: Option<String>,
    client: reqwest::Client,
    codex_home: PathBuf,
}

impl CodexProvider {
    pub fn from_config(_cfg: &crate::config::Config) -> Result<Self> {
        let home = std::env::var("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".codex")))
            .map_err(|_| Error::Config("HOME and CODEX_HOME both unset".into()))?;
        let auth_path = home.join("auth.json");
        let token = if auth_path.exists() {
            let raw = std::fs::read_to_string(&auth_path)?;
            let auth: AuthFile = serde_json::from_str(&raw)
                .map_err(|e| Error::Parse(format!("codex auth.json: {e}")))?;
            auth.tokens.map(|t| t.access_token)
        } else {
            None
        };
        Ok(Self {
            token,
            client: reqwest::Client::builder()
                .user_agent("ai-usage")
                .build()
                .map_err(|e| Error::Http(format!("client build: {e}")))?,
            codex_home: home,
        })
    }
}

#[derive(Debug, Deserialize)]
struct AuthFile {
    tokens: Option<AuthTokens>,
}
#[derive(Debug, Deserialize)]
struct AuthTokens {
    access_token: String,
    #[allow(dead_code)]
    refresh_token: Option<String>,
}

#[async_trait]
impl Provider for CodexProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Codex
    }
    fn configured(&self) -> bool {
        self.token.is_some()
    }

    async fn fetch(&self) -> Result<ProviderReport> {
        let now = time::OffsetDateTime::now_utc();
        let token = match &self.token {
            Some(t) => t.clone(),
            None => {
                return Ok(Self::unavailable(UnavailableReason::NoCredentials, now));
            }
        };

        let resp = match self
            .client
            .get(USAGE_ENDPOINT)
            .bearer_auth(&token)
            .header("Accept", "application/json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(Self::error_report(
                    UnavailableReason::Network,
                    &format!(
                        "wham/usage: {}",
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
                &format!("wham/usage status {status} (token expired? re-run codex login)"),
                now,
            ));
        }
        if !resp.status().is_success() {
            return Ok(Self::error_report(
                UnavailableReason::UnknownEndpoint,
                &format!("wham/usage status {status}"),
                now,
            ));
        }
        let body: WhamUsage = match resp.json().await {
            Ok(b) => b,
            Err(e) => {
                return Ok(Self::error_report(
                    UnavailableReason::Parse,
                    &format!("wham/usage parse: {e}"),
                    now,
                ));
            }
        };

        Ok(self.build(&body, status, now))
    }
}

#[derive(Debug, Deserialize)]
struct WhamUsage {
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    plan_type: Option<String>,
    #[serde(default)]
    rate_limit: Option<RateLimitRoot>,
    #[serde(default)]
    additional_rate_limits: Vec<AdditionalLimit>,
    #[serde(default)]
    credits: Option<Credits>,
    #[serde(default)]
    rate_limit_reset_credits: Option<ResetCredits>,
}

#[derive(Debug, Default, Deserialize)]
struct RateLimitRoot {
    #[serde(default)]
    primary_window: Option<Window>,
    #[serde(default)]
    secondary_window: Option<Window>,
}
#[derive(Debug, Deserialize)]
struct Window {
    used_percent: f64,
    #[serde(default)]
    reset_at: Option<i64>,
}
#[derive(Debug, Deserialize)]
struct AdditionalLimit {
    limit_name: String,
    #[serde(default)]
    rate_limit: Option<RateLimitRoot>,
}
#[derive(Debug, Default, Deserialize)]
#[allow(clippy::struct_field_names)] // `has_credits` matches the upstream wham/usage field name
struct Credits {
    #[serde(default)]
    has_credits: bool,
    #[serde(default)]
    unlimited: bool,
    #[serde(default)]
    balance: Option<String>,
}
#[derive(Debug, Default, Deserialize)]
struct ResetCredits {
    #[serde(default)]
    available_count: Option<u32>,
}

impl CodexProvider {
    fn build(&self, body: &WhamUsage, status: u16, now: time::OffsetDateTime) -> ProviderReport {
        let ttl_60 = Some(time::Duration::seconds(60));

        let mut windows = Vec::new();
        if let Some(rl) = &body.rate_limit {
            push_window(
                &mut windows,
                rl.primary_window.as_ref(),
                WindowKind::Session5h,
                None,
                status,
                now,
            );
            push_window(
                &mut windows,
                rl.secondary_window.as_ref(),
                WindowKind::Weekly,
                None,
                status,
                now,
            );
        }
        for al in &body.additional_rate_limits {
            if let Some(rl) = &al.rate_limit {
                push_window(
                    &mut windows,
                    rl.primary_window.as_ref(),
                    WindowKind::ModelFamily,
                    Some(al.limit_name.clone()),
                    status,
                    now,
                );
                push_window(
                    &mut windows,
                    rl.secondary_window.as_ref(),
                    WindowKind::ModelFamily,
                    Some(al.limit_name.clone()),
                    status,
                    now,
                );
            }
        }

        let banked = body
            .rate_limit_reset_credits
            .as_ref()
            .and_then(|r| r.available_count)
            .map(|count| Sourced {
                value: BankedReset {
                    count,
                    expires_at: None,
                },
                source: Source::live(ENDPOINT, Some(status)),
                observed_at: now,
                ttl: ttl_60,
            });

        let paid_overflow = body.credits.as_ref().map(|c| {
            let balance = c
                .balance
                .as_deref()
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|x| *x > 0.0)
                .map(|v| Money::usd(Decimal::from_f64_retain(v).unwrap_or(Decimal::ZERO)));
            Sourced {
                value: Some(PaidOverflow {
                    enabled: c.has_credits || c.unlimited,
                    balance,
                    spent_this_cycle: None,
                }),
                source: Source::live(ENDPOINT, Some(status)),
                observed_at: now,
                ttl: ttl_60,
            }
        });

        let account = Sourced {
            value: Some(AccountIdentity {
                // Masked so the JSON is safe to share; redactor won't further touch it.
                email_masked: body.email.as_deref().map(crate::view::mask_email),
                plan: body.plan_type.clone(),
            }),
            source: Source::live(ENDPOINT, Some(status)),
            observed_at: now,
            ttl: ttl_60,
        };

        let local_velocity = self.local_velocity(now);

        ProviderReport {
            id: ProviderId::Codex,
            account,
            windows,
            banked_resets: banked.into_iter().collect(),
            plan_capacity: crate::source::unavailable_field(UnavailableReason::NotConfigured),
            paid_overflow: paid_overflow.unwrap_or_else(|| {
                crate::source::unavailable_field(UnavailableReason::NotConfigured)
            }),
            api_balance: crate::source::unavailable_field(UnavailableReason::NotConfigured),
            local_velocity,
            status: ProviderStatus::Ok,
        }
    }

    /// Bounded scan of recent Codex session logs → tokens/hour. `event_msg`
    /// records with `payload.type == "token_count"` carry
    /// `payload.info.total_token_usage.{input,output,cached}_tokens`.
    fn local_velocity(&self, now: time::OffsetDateTime) -> Sourced<Option<LocalVelocity>> {
        let sessions = self.codex_home.join("sessions");
        if !sessions.exists() {
            return crate::source::unavailable_field(UnavailableReason::NotConfigured);
        }
        let cutoff = now - time::Duration::hours(VELOCITY_WINDOW_HOURS as i64);
        let cutoff_systime = SystemTime::from(cutoff);

        let mut total_tokens: u64 = 0;
        let mut samples: u32 = 0;
        crate::walk::recent_jsonl(&sessions, Some(cutoff_systime), 400, &mut |p| {
            // `total_token_usage.total_tokens` is cumulative within a session;
            // the per-session consumption is its max across the file.
            if let Some(session_total) = scan_tokens(p) {
                total_tokens += session_total;
                samples += 1;
            }
        });

        if samples == 0 {
            return crate::source::unavailable_field(UnavailableReason::NotConfigured);
        }
        let burn = total_tokens as f64 / VELOCITY_WINDOW_HOURS as f64;
        Sourced {
            value: Some(LocalVelocity {
                burn_rate_tokens_per_hour: burn,
                window_hours: VELOCITY_WINDOW_HOURS,
                cost_per_hour: None,
                samples,
            }),
            source: Source::LocalLog { path: sessions },
            observed_at: now,
            ttl: Some(time::Duration::seconds(30)),
        }
    }

    fn unavailable(reason: UnavailableReason, now: time::OffsetDateTime) -> ProviderReport {
        use crate::source::unavailable_field as u;
        let _ = now;
        ProviderReport {
            id: ProviderId::Codex,
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

fn push_window(
    out: &mut Vec<Sourced<WindowQuota>>,
    w: Option<&Window>,
    kind: WindowKind,
    label: Option<String>,
    status: u16,
    now: time::OffsetDateTime,
) {
    if let Some(w) = w {
        let reset_at = w
            .reset_at
            .and_then(|s| time::OffsetDateTime::from_unix_timestamp(s).ok());
        out.push(Sourced {
            value: WindowQuota {
                kind,
                label,
                used: (w.used_percent / 100.0).clamp(0.0, 1.0),
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

/// Return the maximum active-token usage (input + output, excluding cached
/// replay and reasoning) seen in a Codex session jsonl — i.e. that session's
/// real consumption. The field is cumulative within a session, so we take the max.
fn scan_tokens(path: &std::path::Path) -> Option<u64> {
    let raw = std::fs::read_to_string(path).ok()?;
    let mut max: Option<u64> = None;
    for line in raw.lines() {
        if !line.contains("\"token_count\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let usage = v
            .get("payload")
            .and_then(|p| p.get("info"))
            .and_then(|i| i.get("total_token_usage"));
        if let Some(u) = usage {
            let input = u
                .get("input_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let output = u
                .get("output_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            // exclude cached_input_tokens (replayed context, not new burn)
            let active = input + output;
            if active > 0 {
                max = Some(max.map_or(active, |m| m.max(active)));
            }
        }
    }
    max
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)] // tests assert exact whole-number utilization values
    use super::*;

    #[test]
    fn parses_wham_usage_live_shape() {
        let raw = serde_json::json!({
            "email": "wielandtrevor@example.com",
            "plan_type": "pro",
            "rate_limit": {
                "primary_window": { "used_percent": 11, "reset_at": 1_783_539_401 },
                "secondary_window": { "used_percent": 9, "reset_at": 1_783_905_876 }
            },
            "additional_rate_limits": [{
                "limit_name": "GPT-5.3-Codex-Spark",
                "metered_feature": "codex_bengalfox",
                "rate_limit": {
                    "primary_window": { "used_percent": 5, "reset_at": 1_783_539_401 }
                }
            }],
            "credits": { "has_credits": false, "unlimited": false, "balance": "0" },
            "rate_limit_reset_credits": { "available_count": 1 }
        });
        let body: WhamUsage = serde_json::from_value(raw).unwrap();
        // build() needs a provider with a dummy home; just assert the parse mapping.
        assert_eq!(
            body.rate_limit
                .unwrap()
                .primary_window
                .unwrap()
                .used_percent,
            11.0
        );
        assert_eq!(body.additional_rate_limits.len(), 1);
        assert_eq!(
            body.rate_limit_reset_credits.unwrap().available_count,
            Some(1)
        );
    }
}
