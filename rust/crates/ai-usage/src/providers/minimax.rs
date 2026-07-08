//! MiniMax provider. See `SPEC.md §2`.
//!
//! MiniMax exposes **no key-only quota endpoint for coding-plan (`sk-cp-*`)
//! subscriptions** — proven: `token_plan/remains` returns `2049 invalid api key`
//! and `coding_plan/remains` returns `1004 cookie is missing`. The coding-plan
//! quota API requires a logged-in browser **session cookie** (the `sk-cp-*`
//! token is for inference only). An `sk-api-*` token-plan key does work on
//! `token_plan/remains`.
//!
//! Resolution:
//!   1. cookie file (configured `cookie_path` or `/run/secrets/minimax-cookie`)
//!      → GET `<host>/v1/api/openplatform/coding_plan/remains` with `Cookie:`
//!      (+ `Authorization: Bearer <key>` when a key is also present)
//!   2. api key only → GET `<host>/v1/token_plan/remains` (Bearer), then the
//!      coding_plan endpoint as fallback
//!
//! Host default `api.minimax.io` (current global MiniMax API host); override via
//! `MINIMAX_HOST`. HTTPS-only. Response is a list of per-model remains
//! (CodexBar's `MiniMaxModelRemains` shape): each has `current_interval_*` and
//! `current_weekly_*` counts + `_remaining_percent` + epoch `_end_time`.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::model::{
    AccountIdentity, ProviderId, ProviderReport, ProviderStatus, WindowKind, WindowQuota,
};
use crate::provider::Provider;
use crate::source::{Source, Sourced, UnavailableReason};

const DEFAULT_HOST: &str = "api.minimax.io";
const CODING_PLAN_PATH: &str = "/v1/api/openplatform/coding_plan/remains";
const TOKEN_PLAN_PATH: &str = "/v1/token_plan/remains";
const ENDPOINT: &str = "minimax.coding-plan-remains";

pub struct MiniMaxProvider {
    cookie: Option<String>,
    token: Option<String>,
    host: String,
    client: reqwest::Client,
}

impl MiniMaxProvider {
    pub fn from_config(cfg: &Config) -> Result<Self> {
        let cookie = cfg.resolve_cookie(
            "minimax",
            cfg.providers
                .minimax
                .as_ref()
                .and_then(|p| p.cookie_path.as_deref()),
        )?;
        let token = cfg.resolve_provider_key(
            "minimax",
            "minimax",
            cfg.providers
                .minimax
                .as_ref()
                .and_then(|p| p.api_key_path.as_deref()),
        )?;
        let host = std::env::var("MINIMAX_HOST")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                crate::config::assert_https(&s)?;
                Ok::<String, Error>(s.trim_end_matches('/').to_string())
            })
            .transpose()?
            .unwrap_or_else(|| DEFAULT_HOST.to_string());
        Ok(Self {
            cookie,
            token,
            host,
            client: reqwest::Client::builder()
                .user_agent("ai-usage")
                .build()
                .map_err(|e| Error::Http(format!("client build: {e}")))?,
        })
    }

    async fn get(&self, path: &str, use_cookie: bool) -> Result<reqwest::Response> {
        let url = format!("https://{}{}", self.host, path);
        let mut req = self.client.get(&url).header("Accept", "application/json");
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        if use_cookie {
            if let Some(c) = &self.cookie {
                req = req.header("Cookie", c);
            }
        }
        req = req.header("MM-API-Source", "ai-usage");
        req.send().await.map_err(|e| {
            Error::Http(format!(
                "minimax: {}",
                crate::redact::Redactor::redact_str(&e.to_string())
            ))
        })
    }
}

#[async_trait]
impl Provider for MiniMaxProvider {
    fn id(&self) -> ProviderId {
        ProviderId::MiniMax
    }
    fn configured(&self) -> bool {
        self.cookie.is_some() || self.token.is_some()
    }

    async fn fetch(&self) -> Result<ProviderReport> {
        let now = time::OffsetDateTime::now_utc();

        if self.cookie.is_none() && self.token.is_none() {
            return Ok(unavailable(
                UnavailableReason::NoCredentials,
                "no cookie file and no api key (coding-plan quota needs a browser cookie; see docs)",
                now,
            ));
        }

        // 1. Cookie path (coding-plan subs): coding_plan/remains with Cookie[+Bearer].
        if self.cookie.is_some() {
            match self.get(CODING_PLAN_PATH, true).await {
                Ok(r) => {
                    let status = r.status().as_u16();
                    if r.status().is_success() {
                        if let Ok(body) = r.json::<Value>().await {
                            if let Some(err) = base_resp_error(&body) {
                                // Cookie present but rejected → clear auth diagnostic.
                                return Ok(unavailable(
                                    UnavailableReason::AuthFailed,
                                    &format!("coding_plan: {err}"),
                                    now,
                                ));
                            }
                            return Ok(build(&body, status, now));
                        }
                    } else if status == 401 || status == 403 {
                        return Ok(unavailable(
                            UnavailableReason::AuthFailed,
                            &format!("status {status}"),
                            now,
                        ));
                    }
                }
                Err(e) => {
                    return Ok(unavailable(
                        UnavailableReason::Network,
                        &crate::redact::Redactor::redact_str(&e.to_string()),
                        now,
                    ));
                }
            }
        }

        // 2. Key-only path (token-plan `sk-api-*` keys): token_plan first, then coding_plan.
        if let Some(_t) = &self.token {
            for path in [TOKEN_PLAN_PATH, CODING_PLAN_PATH] {
                if let Ok(r) = self.get(path, false).await {
                    let status = r.status().as_u16();
                    if r.status().is_success() {
                        if let Ok(body) = r.json::<Value>().await {
                            if let Some(err) = base_resp_error(&body) {
                                // invalid api key / cookie missing → try next endpoint
                                if err.contains("invalid api key") || err.contains("cookie") {
                                    continue;
                                }
                                return Ok(unavailable(UnavailableReason::AuthFailed, &err, now));
                            }
                            return Ok(build(&body, status, now));
                        }
                    }
                }
            }
            return Ok(unavailable(
                UnavailableReason::AuthFailed,
                "api key rejected by token_plan and coding_plan endpoints (coding-plan `sk-cp-*` keys need a browser cookie)",
                now,
            ));
        }

        Ok(unavailable(
            UnavailableReason::AuthFailed,
            "all minimax paths failed",
            now,
        ))
    }
}

/// MiniMax returns `base_resp.status_code` (!= 200 ⇒ error) even on HTTP 200.
fn base_resp_error(body: &Value) -> Option<String> {
    let code = body
        .get("base_resp")
        .and_then(|b| b.get("status_code"))
        .and_then(serde_json::Value::as_i64)?;
    if code == 0 || code == 200 {
        return None;
    }
    let msg = body
        .get("base_resp")
        .and_then(|b| b.get("status_msg"))
        .and_then(|s| s.as_str())
        .unwrap_or("minimax error");
    Some(msg.to_string())
}

/// Per-model remains entry (CodexBar's MiniMaxModelRemains shape).
#[derive(Debug, Default, Deserialize)]
struct ModelRemains {
    #[serde(default, rename = "model_name")]
    model_name: Option<String>,
    // interval (session-like) window
    #[serde(default, rename = "current_interval_total_count")]
    interval_total: Option<f64>,
    #[serde(default, rename = "current_interval_usage_count")]
    interval_used: Option<f64>,
    #[serde(default, rename = "current_interval_remaining_percent")]
    interval_remaining_pct: Option<f64>,
    #[serde(default, rename = "end_time")]
    interval_end: Option<i64>,
    // weekly window
    #[serde(default, rename = "current_weekly_total_count")]
    weekly_total: Option<f64>,
    #[serde(default, rename = "current_weekly_usage_count")]
    weekly_used: Option<f64>,
    #[serde(default, rename = "current_weekly_remaining_percent")]
    weekly_remaining_pct: Option<f64>,
    #[serde(default, rename = "weekly_end_time")]
    weekly_end: Option<i64>,
}

fn build(body: &Value, status: u16, now: time::OffsetDateTime) -> ProviderReport {
    use crate::source::unavailable_field;
    let ttl = Some(time::Duration::seconds(60));

    let entries = extract_remains(body);
    let mut windows = Vec::new();
    for e in &entries {
        if let Some(w) = window_from(e, Slot::Interval, WindowKind::Session5h, status, now, ttl) {
            windows.push(w);
        }
        if let Some(w) = window_from(e, Slot::Weekly, WindowKind::Weekly, status, now, ttl) {
            windows.push(w);
        }
    }

    let plan = body
        .get("data")
        .and_then(|d| d.get("plan_name"))
        .and_then(|p| p.as_str())
        .map(std::string::ToString::to_string);
    let account = Sourced {
        value: Some(AccountIdentity {
            email_masked: None,
            plan,
        }),
        source: Source::live(ENDPOINT, Some(status)),
        observed_at: now,
        ttl,
    };

    let degraded = windows.is_empty();
    ProviderReport {
        id: ProviderId::MiniMax,
        account,
        windows,
        banked_resets: vec![],
        plan_capacity: unavailable_field(UnavailableReason::NotConfigured),
        paid_overflow: unavailable_field(UnavailableReason::NotConfigured),
        api_balance: unavailable_field(UnavailableReason::NotConfigured),
        local_velocity: unavailable_field(UnavailableReason::NotConfigured),
        status: if degraded {
            ProviderStatus::Degraded {
                notes: vec!["coding_plan response had no recognizable model windows".into()],
            }
        } else {
            ProviderStatus::Ok
        },
    }
}

/// Find the list of model-remains entries across plausible envelopes.
fn extract_remains(body: &Value) -> Vec<ModelRemains> {
    let candidates: Vec<&Value> = [
        body.get("data").and_then(|d| d.get("model_remains")),
        body.get("data").and_then(|d| d.get("remains")),
        body.get("data").and_then(|d| d.get("services")),
        body.get("model_remains"),
        body.get("remains"),
        body.as_array().map(|_| body),
    ]
    .into_iter()
    .flatten()
    .collect();
    for c in candidates {
        if let Some(arr) = c.as_array() {
            let parsed: Vec<ModelRemains> = arr
                .iter()
                .filter_map(|v| serde_json::from_value(v.clone()).ok())
                .collect();
            if !parsed.is_empty() {
                return parsed;
            }
        }
    }
    Vec::new()
}

#[derive(Clone, Copy)]
enum Slot {
    Interval,
    Weekly,
}

fn window_from(
    e: &ModelRemains,
    slot: Slot,
    kind: WindowKind,
    status: u16,
    now: time::OffsetDateTime,
    ttl: Option<time::Duration>,
) -> Option<Sourced<WindowQuota>> {
    let (total, used, remaining_pct, end_time) = match slot {
        Slot::Interval => (
            e.interval_total,
            e.interval_used,
            e.interval_remaining_pct,
            e.interval_end,
        ),
        Slot::Weekly => (
            e.weekly_total,
            e.weekly_used,
            e.weekly_remaining_pct,
            e.weekly_end,
        ),
    };
    // Prefer counts; fall back to (1 - remaining_percent/100).
    let used_ratio = match (used, total) {
        (Some(u), Some(t)) if t > 0.0 => Some((u / t).clamp(0.0, 1.0)),
        _ => remaining_pct.map(|r| (1.0 - r / 100.0).clamp(0.0, 1.0)),
    }?;
    let reset_at = end_time.and_then(|t| {
        // end_time may be seconds or milliseconds; pick the sane range.
        let secs = if t > 1_000_000_000_000 { t / 1000 } else { t };
        time::OffsetDateTime::from_unix_timestamp(secs).ok()
    });
    Some(Sourced {
        value: WindowQuota {
            kind,
            label: e.model_name.clone(),
            used: used_ratio,
            limit: total.map(|t| t as u64),
            used_count: used.map(|u| u as u64),
            reset_at,
            rolling: matches!(kind, WindowKind::Session5h),
        },
        source: Source::live(ENDPOINT, Some(status)),
        observed_at: now,
        ttl,
    })
}

fn unavailable(reason: UnavailableReason, msg: &str, now: time::OffsetDateTime) -> ProviderReport {
    use crate::source::unavailable_field as u;
    let _ = now;
    ProviderReport {
        id: ProviderId::MiniMax,
        account: u(reason),
        windows: vec![],
        banked_resets: vec![],
        plan_capacity: u(reason),
        paid_overflow: u(reason),
        api_balance: u(reason),
        local_velocity: u(reason),
        status: ProviderStatus::Unavailable {
            reason: crate::redact::Redactor::redact_str(msg),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_model_remains_live_shape() {
        let body = serde_json::json!({
            "base_resp": {"status_code": 0},
            "data": {
                "model_remains": [{
                    "model_name": "abab6.5s",
                    "current_interval_total_count": 10000,
                    "current_interval_usage_count": 2500,
                    "current_interval_remaining_percent": 75,
                    "end_time": 1_784_142_970,
                    "current_weekly_total_count": 50000,
                    "current_weekly_usage_count": 5000,
                    "current_weekly_remaining_percent": 90,
                    "weekly_end_time": 1_784_661_370
                }]
            }
        });
        let entries = extract_remains(&body);
        assert_eq!(entries.len(), 1);
        let report = build(&body, 200, time::OffsetDateTime::now_utc());
        // interval + weekly windows
        assert_eq!(report.windows.len(), 2);
        let session = report
            .windows
            .iter()
            .find(|w| w.value.kind == WindowKind::Session5h)
            .unwrap();
        assert!((session.value.used - 0.25).abs() < 1e-6);
    }

    #[test]
    fn base_resp_error_detected() {
        let body = serde_json::json!({"base_resp":{"status_code":1004,"status_msg":"cookie is missing, log in again"}});
        assert_eq!(
            base_resp_error(&body).as_deref(),
            Some("cookie is missing, log in again")
        );
        let ok = serde_json::json!({"base_resp":{"status_code":200}});
        assert!(base_resp_error(&ok).is_none());
        let current_ok = serde_json::json!({"base_resp":{"status_code":0,"status_msg":"success"}});
        assert!(base_resp_error(&current_ok).is_none());
    }
}
