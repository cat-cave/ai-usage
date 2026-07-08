//! Grok (xAI / SuperGrok) provider. See `SPEC.md §2`.
//!
//! `~/.grok/auth.json` holds the SuperGrok OIDC bearer (top-level key is the
//! scope URL `https://auth.x.ai::<client-id>`). Usage comes from grok.com's
//! gRPC-web billing endpoint
//!   `POST https://grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig`
//! using the bearer token alone (no browser cookies required). The response is a
//! small protobuf; we scan it (porting CodexBar's algorithm) to recover the used
//! percent and reset timestamp. Local velocity comes from
//! `~/.grok/sessions/**/signals.json`.
//!
//! `grok agent stdio` exposes an `x.ai/billing` JSON-RPC method, but as of
//! grok 0.2.91 it still returns `-32601 Method not found` on the stdio surface
//! (only wired in the interactive TUI), so the gRPC-web path is the live one.

use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::model::{
    AccountIdentity, LocalVelocity, ProviderId, ProviderReport, ProviderStatus, WindowKind,
    WindowQuota,
};
use crate::provider::Provider;
use crate::source::{Source, Sourced, UnavailableReason};

const BILLING_ENDPOINT: &str = "https://grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig";
const ENDPOINT: &str = "grok.grpc-web-billing";
const VELOCITY_WINDOW_HOURS: u64 = 24;
const GRPC_REQUEST_BODY: [u8; 5] = [0, 0, 0, 0, 0];

pub struct GrokProvider {
    creds: Option<Creds>,
    client: reqwest::Client,
    grok_home: PathBuf,
}

#[derive(Clone)]
struct Creds {
    token: String,
    email: Option<String>,
    #[allow(dead_code)]
    team_id: Option<String>,
    auth_mode: Option<String>,
    expires_at: Option<time::OffsetDateTime>,
}

impl GrokProvider {
    pub fn from_config(_cfg: &crate::config::Config) -> Result<Self> {
        let grok_home = std::env::var("GROK_HOME")
            .map(PathBuf::from)
            .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".grok")))
            .map_err(|_| Error::Config("HOME and GROK_HOME both unset".into()))?;
        let auth_path = grok_home.join("auth.json");
        let creds = if auth_path.exists() {
            let raw = std::fs::read_to_string(&auth_path)?;
            let map: HashMap<String, AuthEntry> = serde_json::from_str(&raw)
                .map_err(|e| Error::Parse(format!("grok auth.json: {e}")))?;
            // Prefer the SuperGrok OIDC entry; fall back to the first.
            let entry = map
                .iter()
                .find(|(k, _)| k.starts_with("https://auth.x.ai::"))
                .or_else(|| map.iter().next())
                .map(|(_, v)| v.clone());
            entry.map(|e| Creds {
                token: e.key,
                email: e.email,
                team_id: e.team_id,
                auth_mode: e.auth_mode,
                expires_at: e.expires_at.as_deref().and_then(|s| {
                    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
                        .ok()
                }),
            })
        } else {
            None
        };
        Ok(Self {
            creds,
            client: reqwest::Client::builder()
                .user_agent("ai-usage")
                .build()
                .map_err(|e| Error::Http(format!("client build: {e}")))?,
            grok_home,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
struct AuthEntry {
    key: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    team_id: Option<String>,
    #[serde(default)]
    auth_mode: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
}

#[async_trait]
impl Provider for GrokProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Grok
    }
    fn configured(&self) -> bool {
        self.creds.is_some()
    }

    async fn fetch(&self) -> Result<ProviderReport> {
        let now = time::OffsetDateTime::now_utc();
        let creds = match &self.creds {
            Some(c) if !c.is_expired() => c.clone(),
            Some(_) => {
                return Ok(Self::error_report(
                    UnavailableReason::AuthFailed,
                    "grok token expired (run `grok login`)",
                    now,
                ));
            }
            None => return Ok(Self::unavailable(UnavailableReason::NoCredentials, now)),
        };

        let resp = match self
            .client
            .post(BILLING_ENDPOINT)
            .header("authorization", format!("Bearer {}", creds.token))
            .header("content-type", "application/grpc-web+proto")
            .header("x-grpc-web", "1")
            .header("x-user-agent", "connect-es/2.1.1")
            .header("origin", "https://grok.com")
            .header("referer", "https://grok.com/?_s=usage")
            .body(GRPC_REQUEST_BODY.to_vec())
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(Self::error_report(
                    UnavailableReason::Network,
                    &format!(
                        "billing: {}",
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
                &format!("billing status {status} (run `grok login`)"),
                now,
            ));
        }
        if status != 200 {
            return Ok(Self::error_report(
                UnavailableReason::UnknownEndpoint,
                &format!("billing status {status}"),
                now,
            ));
        }
        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                return Ok(Self::error_report(
                    UnavailableReason::Parse,
                    &format!("billing body: {e}"),
                    now,
                ));
            }
        };

        // gRPC-web trailers carry grpc-status; non-zero means RPC failure.
        if let Some(code) = grpc_status(&bytes) {
            if code != 0 {
                return Ok(Self::error_report(
                    UnavailableReason::AuthFailed,
                    &format!("grpc-status {code}"),
                    now,
                ));
            }
        }

        let snapshot = parse_billing(&bytes, now);
        Ok(self.build(&snapshot, status, &creds, now))
    }
}

impl GrokProvider {
    fn build(
        &self,
        snap: &BillingSnapshot,
        status: u16,
        creds: &Creds,
        now: time::OffsetDateTime,
    ) -> ProviderReport {
        let ttl = Some(time::Duration::seconds(60));
        let mut windows = Vec::new();
        if let Some(pct) = snap.used_percent {
            windows.push(Sourced {
                value: WindowQuota {
                    kind: WindowKind::Weekly,
                    label: Some("credits".into()),
                    used: (pct / 100.0).clamp(0.0, 1.0),
                    limit: None,
                    used_count: None,
                    reset_at: snap.resets_at,
                    rolling: false,
                },
                source: Source::live(ENDPOINT, Some(status)),
                observed_at: now,
                ttl,
            });
        }

        let plan = creds
            .auth_mode
            .clone()
            .filter(|m| m == "oidc")
            .map(|_| "SuperGrok".to_string())
            .or_else(|| creds.auth_mode.clone());

        let account = Sourced {
            value: Some(AccountIdentity {
                email_masked: creds.email.as_deref().map(crate::view::mask_email),
                plan,
            }),
            source: Source::live(ENDPOINT, Some(status)),
            observed_at: now,
            ttl,
        };

        let local_velocity = self.local_velocity(now);

        ProviderReport {
            id: ProviderId::Grok,
            account,
            windows,
            banked_resets: vec![],
            plan_capacity: crate::source::unavailable_field(UnavailableReason::NotConfigured),
            paid_overflow: crate::source::unavailable_field(UnavailableReason::NotConfigured),
            api_balance: crate::source::unavailable_field(UnavailableReason::NotConfigured),
            local_velocity,
            status: ProviderStatus::Ok,
        }
    }

    fn local_velocity(&self, now: time::OffsetDateTime) -> Sourced<Option<LocalVelocity>> {
        let sessions = self.grok_home.join("sessions");
        if !sessions.exists() {
            return crate::source::unavailable_field(UnavailableReason::NotConfigured);
        }
        let cutoff = now - time::Duration::hours(VELOCITY_WINDOW_HOURS as i64);
        let cutoff_st = std::time::SystemTime::from(cutoff);
        let mut total_tokens: u64 = 0;
        let mut samples: u32 = 0;
        crate::walk::recent_files(&sessions, "json", Some(cutoff_st), 400, &mut |p| {
            if let Some(t) = scan_signals(p) {
                total_tokens += t;
                samples += 1;
            }
        });
        if samples == 0 {
            return crate::source::unavailable_field(UnavailableReason::NotConfigured);
        }
        Sourced {
            value: Some(LocalVelocity {
                burn_rate_tokens_per_hour: total_tokens as f64 / VELOCITY_WINDOW_HOURS as f64,
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
            id: ProviderId::Grok,
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

impl Creds {
    fn is_expired(&self) -> bool {
        self.expires_at
            .is_some_and(|exp| time::OffsetDateTime::now_utc() >= exp)
    }
}

#[derive(Default)]
struct BillingSnapshot {
    used_percent: Option<f64>,
    resets_at: Option<time::OffsetDateTime>,
}

/// Parse the gRPC-web billing response, porting CodexBar's algorithm:
/// - used_percent = shallowest fixed32 whose path ends in field 1 and value ∈ [0,100]
/// - resets_at = varint in the Unix-timestamp range, preferring path [1,5,1]
/// - if no fixed32 but a valid period exists, percent defaults to 0
fn parse_billing(data: &[u8], now: time::OffsetDateTime) -> BillingSnapshot {
    let payloads = grpc_web_data_frames(data);
    let mut scan = ProtoScan::default();
    for p in &payloads {
        scan.merge(scan_protobuf(p, 0, &[]));
    }

    let used_percent = scan
        .fixed32
        .iter()
        .filter(|f| {
            *f.path.last().unwrap_or(&0) == 1
                && f.value.is_finite()
                && (0.0..=100.0).contains(&f.value)
        })
        .min_by_key(|f| f.path.len())
        .map(|f| f64::from(f.value));

    let future_resets: Vec<time::OffsetDateTime> = scan
        .varints
        .iter()
        .filter_map(|f| {
            let v = f.value;
            if (1_700_000_000..=2_100_000_000).contains(&v) {
                let t = time::OffsetDateTime::from_unix_timestamp(v as i64).ok()?;
                if t > now {
                    Some(t)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    let resets_at = scan
        .varints
        .iter()
        .find(|f| f.path == vec![1u32, 5, 1] && (1_700_000_000..=2_100_000_000).contains(&f.value))
        .and_then(|f| time::OffsetDateTime::from_unix_timestamp(f.value as i64).ok())
        .or_else(|| future_resets.into_iter().min());

    // No fixed32 + a valid period → fresh cycle, zero usage.
    let has_period = scan
        .varints
        .iter()
        .any(|f| f.path.first() == Some(&1) && f.path.get(1) == Some(&6))
        || scan
            .varints
            .iter()
            .any(|f| f.path == vec![1u32, 8, 1] && (f.value == 1 || f.value == 2));

    let used_percent = match (
        used_percent,
        scan.fixed32.is_empty(),
        resets_at.is_some(),
        has_period,
    ) {
        (Some(p), _, _, _) => Some(p),
        (None, true, true, true) => Some(0.0),
        _ => None,
    };

    BillingSnapshot {
        used_percent,
        resets_at,
    }
}

/// gRPC-web framing: each frame is `[flags:1][len:4 BE][payload]`. Data frames
/// have the top bit of flags clear.
fn grpc_web_data_frames(data: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut i = 0;
    while i + 5 <= data.len() {
        let flags = data[i];
        let len = (u32::from(data[i + 1]) << 24
            | u32::from(data[i + 2]) << 16
            | u32::from(data[i + 3]) << 8
            | u32::from(data[i + 4])) as usize;
        let start = i + 5;
        if len > data.len() - start {
            break;
        }
        if flags & 0x80 == 0 {
            frames.push(data[start..start + len].to_vec());
        }
        i = start + len;
    }
    frames
}

/// Trailer frames (flags top bit set) carry `grpc-status: <code>` text.
fn grpc_status(data: &[u8]) -> Option<i64> {
    let mut i = 0;
    while i + 5 <= data.len() {
        let flags = data[i];
        let len = (u32::from(data[i + 1]) << 24
            | u32::from(data[i + 2]) << 16
            | u32::from(data[i + 3]) << 8
            | u32::from(data[i + 4])) as usize;
        let start = i + 5;
        if len > data.len() - start {
            break;
        }
        if flags & 0x80 != 0 {
            if let Ok(text) = std::str::from_utf8(&data[start..start + len]) {
                for line in text.lines() {
                    if let Some(rest) = line.trim().strip_prefix("grpc-status:") {
                        if let Ok(code) = rest.trim().parse::<i64>() {
                            return Some(code);
                        }
                    }
                }
            }
        }
        i = start + len;
    }
    None
}

#[derive(Default)]
struct ProtoScan {
    fixed32: Vec<Fixed32>,
    varints: Vec<Varint>,
}
struct Fixed32 {
    path: Vec<u32>,
    value: f32,
}
struct Varint {
    path: Vec<u32>,
    value: u64,
}

impl ProtoScan {
    fn merge(&mut self, other: Self) {
        self.fixed32.extend(other.fixed32);
        self.varints.extend(other.varints);
    }
}

fn scan_protobuf(data: &[u8], depth: usize, path: &[u32]) -> ProtoScan {
    let mut scan = ProtoScan::default();
    let mut i = 0;
    let read_var = |i: &mut usize| -> Option<u64> {
        let mut shift = 0u32;
        let mut value: u64 = 0;
        while *i < data.len() && shift < 64 {
            let b = data[*i];
            *i += 1;
            value |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Some(value);
            }
            shift += 7;
        }
        None
    };
    while i < data.len() {
        let start = i;
        let Some(tag) = read_var(&mut i) else {
            i = start + 1;
            continue;
        };
        if tag == 0 {
            i = start + 1;
            continue;
        }
        let field = (tag >> 3) as u32;
        let wire = (tag & 0x07) as u8;
        let mut fp = path.to_vec();
        fp.push(field);
        match wire {
            0 => {
                if let Some(v) = read_var(&mut i) {
                    scan.varints.push(Varint { path: fp, value: v });
                } else {
                    i = start + 1;
                }
            }
            1 => {
                if i + 8 > data.len() {
                    break;
                }
                i += 8;
            }
            2 => {
                let Some(len) = read_var(&mut i) else {
                    i = start + 1;
                    continue;
                };
                let len = len as usize;
                if len > data.len() - i {
                    i = start + 1;
                    continue;
                }
                if depth < 4 {
                    let nested = scan_protobuf(&data[i..i + len], depth + 1, &fp);
                    scan.merge(nested);
                }
                i += len;
            }
            5 => {
                if i + 4 > data.len() {
                    break;
                }
                let bits = u32::from(data[i])
                    | (u32::from(data[i + 1]) << 8)
                    | (u32::from(data[i + 2]) << 16)
                    | (u32::from(data[i + 3]) << 24);
                scan.fixed32.push(Fixed32 {
                    path: fp,
                    value: f32::from_bits(bits),
                });
                i += 4;
            }
            _ => {
                i = start + 1;
            }
        }
    }
    scan
}

/// Sum `contextTokensUsed + totalTokensBeforeCompaction` from a grok signals.json.
fn scan_signals(path: &std::path::Path) -> Option<u64> {
    let raw = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let ctx = v
        .get("contextTokensUsed")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let pre = v
        .get("totalTokensBeforeCompaction")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let total = ctx + pre;
    if total > 0 {
        Some(total)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_grpc_web_response() {
        // A minimal frame echoing the real shape: nested messages carrying two
        // unix-timestamp varints (period start [1,4,1] and reset [1,5,1]) and a
        // usage-period marker [1,8,1]=2, with no fixed32 → fresh cycle, 0%.
        let inner = build_nested();
        let mut framed = vec![0u8]; // data flag
        framed.extend_from_slice(&(inner.len() as u32).to_be_bytes());
        framed.extend_from_slice(&inner);
        let now = time::OffsetDateTime::from_unix_timestamp(1_784_000_000).unwrap();
        let snap = parse_billing(&framed, now);
        assert_eq!(snap.used_percent, Some(0.0));
        // [1,5,1] is the preferred reset path.
        assert!(snap.resets_at.is_some());
    }

    fn build_nested() -> Vec<u8> {
        // Hand-rolled protobuf: field 1 { field 4 {1:<ts>,2:<n>}, field 5 {1:<ts>,2:<n>}, field 8 {1:2} }
        fn varint(v: u64) -> Vec<u8> {
            let mut out = Vec::new();
            let mut v = v;
            while v >= 0x80 {
                out.push((v as u8 & 0x7f) | 0x80);
                v >>= 7;
            }
            out.push(v as u8);
            out
        }
        fn tag(field: u32, wire: u8) -> Vec<u8> {
            varint((u64::from(field) << 3) | u64::from(wire))
        }
        fn msg(fields: Vec<u8>) -> Vec<u8> {
            fields
        }
        let ts1 = varint(1_783_538_170u64);
        let ts2 = varint(1_784_142_970u64);
        let n = varint(608_393_000u64);
        let f4 = msg({
            let mut m = tag(1, 0);
            m.extend(&ts1);
            m.extend(&tag(2, 0));
            m.extend(&n);
            m
        });
        let f5 = msg({
            let mut m = tag(1, 0);
            m.extend(&ts2);
            m.extend(&tag(2, 0));
            m.extend(&n);
            m
        });
        let f8 = msg({
            let mut m = tag(1, 0);
            m.extend(&varint(2));
            m
        });
        let mut inner = tag(4, 2);
        inner.extend(&varint(f4.len() as u64));
        inner.extend(&f4);
        inner.extend(&tag(5, 2));
        inner.extend(&varint(f5.len() as u64));
        inner.extend(&f5);
        inner.extend(&tag(8, 2));
        inner.extend(&varint(f8.len() as u64));
        inner.extend(&f8);
        let mut top = tag(1, 2);
        top.extend(&varint(inner.len() as u64));
        top.extend(&inner);
        top
    }
}
