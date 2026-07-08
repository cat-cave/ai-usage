//! Internal test helpers. Compiled only under `#[cfg(test)]`.

#![cfg(test)]

use time::OffsetDateTime;

use crate::model::{
    AccountIdentity, ProviderId, ProviderReport, ProviderStatus, WindowKind, WindowQuota,
};
use crate::source::{Source, Sourced, UnavailableReason};

pub fn empty_report(id: ProviderId) -> ProviderReport {
    unavailable_report(id, UnavailableReason::NotConfigured)
}

pub fn unavailable_report(id: ProviderId, reason: UnavailableReason) -> ProviderReport {
    use crate::source::unavailable_field as u;
    ProviderReport {
        id,
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

/// A live provider with one window at `used` (0.0–1.0), resetting at `reset_at`.
pub fn live_windowed_report(
    id: ProviderId,
    kind: WindowKind,
    used: f64,
    reset_at: Option<OffsetDateTime>,
) -> ProviderReport {
    let now = OffsetDateTime::now_utc();
    use crate::source::unavailable_field as u;
    ProviderReport {
        id,
        account: Sourced {
            value: Some(AccountIdentity {
                email_masked: None,
                plan: None,
            }),
            source: Source::live("test.endpoint", Some(200)),
            observed_at: now,
            ttl: Some(time::Duration::seconds(60)),
        },
        windows: vec![Sourced {
            value: WindowQuota {
                kind,
                label: None,
                used,
                limit: None,
                used_count: None,
                reset_at,
                rolling: matches!(kind, WindowKind::Session5h),
            },
            source: Source::live("test.endpoint", Some(200)),
            observed_at: now,
            ttl: Some(time::Duration::seconds(60)),
        }],
        banked_resets: vec![],
        plan_capacity: u(UnavailableReason::NotConfigured),
        paid_overflow: u(UnavailableReason::NotConfigured),
        api_balance: u(UnavailableReason::NotConfigured),
        local_velocity: u(UnavailableReason::NotConfigured),
        status: ProviderStatus::Ok,
    }
}
