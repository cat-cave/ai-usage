//! Provenance: every datum is traceable to a source. See `SPEC.md §1, §3`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Why a datum is missing. `Unavailable` is a first-class value, never `0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum UnavailableReason {
    NoCredentials,
    NotConfigured,
    AuthFailed,
    Network,
    Parse,
    EndpointDisabled,
    ScopeMissing,
    UnknownEndpoint,
    Disabled,
}

/// Where a value came from. Never contains the credential itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    /// A real provider API call. `endpoint` is a static label, not a URL with secrets.
    LiveApi {
        endpoint: String,
        status_code: Option<u16>,
    },
    /// Derived from a local provider log (ccusage-style velocity).
    LocalLog { path: PathBuf },
    /// Spawned a provider CLI and parsed its output.
    CliProbe { tool: String },
    /// Served from cache.
    Cached,
    /// Explicit user/env config (auth identity, region, plan label only — never quota values).
    Config,
    /// The datum is not available. Reported as a risk, never as zero.
    Unavailable { reason: UnavailableReason },
}

impl Source {
    pub const fn unavailable(reason: UnavailableReason) -> Self {
        Self::Unavailable { reason }
    }

    /// Convenience constructor for a live-api source label.
    pub fn live(endpoint: impl Into<String>, status_code: Option<u16>) -> Self {
        Self::LiveApi {
            endpoint: endpoint.into(),
            status_code,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    Live,
    Fresh,
    Stale,
    Unknown,
}

impl Source {
    /// Best-effort freshness classification for a value with a `ttl` and `observed_at`.
    pub fn freshness(&self, observed_at: OffsetDateTime, ttl: Option<time::Duration>) -> Freshness {
        match self {
            Self::Unavailable { .. } | Self::Config => Freshness::Unknown,
            Self::Cached => Freshness::Stale,
            Self::LiveApi { .. } | Self::LocalLog { .. } | Self::CliProbe { .. } => {
                let now = OffsetDateTime::now_utc();
                ttl.map_or(Freshness::Fresh, |ttl| {
                    if now > observed_at + ttl {
                        Freshness::Stale
                    } else {
                        Freshness::Fresh
                    }
                })
            }
        }
    }
}

/// A value paired with its provenance. No quota/balance value leaves a provider
/// adapter without being wrapped in this.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sourced<T> {
    pub value: T,
    pub source: Source,
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<time::Duration>,
}

impl<T> Sourced<T> {
    pub fn new_live(value: T, endpoint: impl Into<String>) -> Self {
        Self {
            value,
            source: Source::LiveApi {
                endpoint: endpoint.into(),
                status_code: None,
            },
            observed_at: OffsetDateTime::now_utc(),
            ttl: Some(time::Duration::seconds(60)),
        }
    }

    pub fn unavailable(reason: UnavailableReason) -> Sourced<Option<T>> {
        Sourced {
            value: None,
            source: Source::unavailable(reason),
            observed_at: OffsetDateTime::now_utc(),
            ttl: None,
        }
    }
}

/// Construct an unavailable `Sourced<Option<T>>` for any field type.
pub fn unavailable_field<T>(reason: UnavailableReason) -> Sourced<Option<T>> {
    Sourced::unavailable(reason)
}

/// A whole field is present (with provenance) or absent with a reason.
pub type OptionalSourced<T> = Sourced<Option<T>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_is_unavailable() {
        let s: OptionalSourced<u64> = Sourced::unavailable(UnavailableReason::NoCredentials);
        match s.source {
            Source::Unavailable { reason } => {
                assert_eq!(reason, UnavailableReason::NoCredentials);
            }
            _ => panic!("expected unavailable"),
        }
        assert_eq!(s.value, None);
    }
}
