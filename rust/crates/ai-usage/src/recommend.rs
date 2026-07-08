//! Recommendation engine. See `SPEC.md §5`.
//!
//! Per-provider score in `[0,1]` from weighted sub-scores:
//! `capacity`, `velocity_fit`, `paid_ok`, `freshness`, `task_fit`.
//! Every recommendation emits human reasons + machine weights.

use serde::{Deserialize, Serialize};

use crate::model::{ProviderId, ProviderReport, ProviderStatus, WindowKind};
use crate::report::AggregateReport;
use crate::source::{Freshness, OptionalSourced, Source};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskKind {
    Short,
    LongCoding,
    Exploratory,
    Review,
    HighContext,
    Audit,
}

impl TaskKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "short" => Self::Short,
            "long-coding" | "long" => Self::LongCoding,
            "exploratory" => Self::Exploratory,
            "review" => Self::Review,
            "high-context" => Self::HighContext,
            "audit" => Self::Audit,
            _ => return None,
        })
    }

    pub const fn kebab(&self) -> &'static str {
        match self {
            Self::Short => "short",
            Self::LongCoding => "long-coding",
            Self::Exploratory => "exploratory",
            Self::Review => "review",
            Self::HighContext => "high-context",
            Self::Audit => "audit",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recommendation {
    pub rank: u32,
    pub provider: ProviderId,
    pub score: f64,
    pub subscores: Subscores,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscores {
    pub capacity: f64,
    pub velocity_fit: f64,
    pub paid_ok: f64,
    pub freshness: f64,
    pub task_fit: f64,
}

/// Rank providers for a task. Best first.
pub fn recommend(report: &AggregateReport, task: TaskKind) -> Vec<Recommendation> {
    let mut recs: Vec<Recommendation> = report
        .providers
        .iter()
        .map(|p| score_one(p, task))
        .collect();
    recs.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (i, r) in recs.iter_mut().enumerate() {
        r.rank = (i + 1) as u32;
    }
    recs
}

fn score_one(p: &ProviderReport, task: TaskKind) -> Recommendation {
    let capacity = capacity_score(p);
    let velocity_fit = velocity_score(p);
    let paid_ok = paid_score(p);
    let freshness = freshness_score(p);
    let task_fit = task_fit_score(p, task);

    // Weighted blend. Freshness acts as a multiplier cap when Unknown/Stale.
    let raw = 0.18f64.mul_add(
        1.0,
        0.10f64.mul_add(
            task_fit,
            0.18f64.mul_add(paid_ok, 0.20f64.mul_add(velocity_fit, 0.34 * capacity)),
        ),
    );
    let score = (raw * freshness).clamp(0.0, 1.0);

    let reasons = reasons(
        p,
        task,
        &Subscores {
            capacity,
            velocity_fit,
            paid_ok,
            freshness,
            task_fit,
        },
    );

    Recommendation {
        rank: 0,
        provider: p.id,
        score: (score * 100.0).round() / 100.0,
        subscores: Subscores {
            capacity: round2(capacity),
            velocity_fit: round2(velocity_fit),
            paid_ok: round2(paid_ok),
            freshness: round2(freshness),
            task_fit: round2(task_fit),
        },
        reasons,
    }
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// `1 - max(used)` across windows, weighted by reset urgency.
fn capacity_score(p: &ProviderReport) -> f64 {
    if p.windows.is_empty() {
        // Pure-API providers have no windowed cap; treat as wide-open capacity.
        return 0.9;
    }
    let worst = p
        .windows
        .iter()
        .map(|w| {
            let used = w.value.used;
            // windows with a near reset get a small grace bump
            let grace = reset_grace(w.value.reset_at);
            (1.0 - used + grace).clamp(0.0, 1.0)
        })
        .fold(f64::INFINITY, f64::min);
    worst
}

fn reset_grace(reset_at: Option<time::OffsetDateTime>) -> f64 {
    reset_at.map_or(0.0, |t| {
        if t <= time::OffsetDateTime::now_utc() {
            0.1 // just reset
        } else {
            0.0
        }
    })
}

const fn velocity_score(p: &ProviderReport) -> f64 {
    match &p.local_velocity.value {
        // Without remaining-capacity numbers we cannot do a full projection;
        // credit observed velocity data as a mild positive (data > no data).
        Some(v) if v.samples > 0 => 0.7,
        _ => 0.5,
    }
}

const fn paid_score(p: &ProviderReport) -> f64 {
    match &p.paid_overflow.value {
        Some(po) if po.enabled => 0.9,
        Some(_) => 0.3,
        None => 0.4, // unknown → neutral risk
    }
}

fn freshness_score(p: &ProviderReport) -> f64 {
    // Worst freshness across the fields we actually used.
    let mut worst = 1.0f64;
    for f in field_freshness(p) {
        worst = worst.min(match f {
            Freshness::Live => 1.0,
            Freshness::Fresh => 0.9,
            Freshness::Stale => 0.5,
            Freshness::Unknown => 0.25,
        });
    }
    worst
}

fn field_freshness(p: &ProviderReport) -> Vec<Freshness> {
    let mut out = Vec::new();
    for w in &p.windows {
        out.push(w.source.freshness(w.observed_at, w.ttl));
    }
    push_opt(&mut out, &p.paid_overflow);
    push_opt(&mut out, &p.api_balance);
    push_opt(&mut out, &p.local_velocity);
    if out.is_empty() {
        out.push(Freshness::Unknown);
    }
    out
}

/// Only an *available* field contributes to freshness. An `Unavailable` field
/// means "no data here" — it must not make fresh window data look stale.
fn push_opt<T>(out: &mut Vec<Freshness>, s: &OptionalSourced<T>) {
    match &s.source {
        Source::Unavailable { .. } => {} // skip: not contributing
        _ => out.push(s.source.freshness(s.observed_at, s.ttl)),
    }
}

fn task_fit_score(p: &ProviderReport, task: TaskKind) -> f64 {
    // Heuristic archetypes. Tunable in a later phase; documented in docs/recommender.md.
    let has_session = p
        .windows
        .iter()
        .any(|w| matches!(w.value.kind, WindowKind::Session5h));
    match (p.id, task) {
        (ProviderId::Codex, TaskKind::LongCoding) | (ProviderId::Claude, TaskKind::Review) => 0.95,
        (ProviderId::Codex | ProviderId::Grok, TaskKind::Exploratory) => 0.8,
        (ProviderId::Claude, TaskKind::HighContext)
        | (ProviderId::Grok, TaskKind::LongCoding | TaskKind::HighContext) => 0.85,
        (ProviderId::Claude, TaskKind::LongCoding) if has_session => 0.6,
        (ProviderId::OpenRouter, TaskKind::Audit)
        | (ProviderId::DeepSeek, TaskKind::LongCoding) => 0.7,
        (ProviderId::OpenRouter, _) => 0.65,
        (ProviderId::DeepSeek | ProviderId::Grok, _) => 0.6,
        _ => 0.5,
    }
}

fn reasons(p: &ProviderReport, _task: TaskKind, s: &Subscores) -> Vec<String> {
    let mut out = Vec::new();
    if matches!(p.status, ProviderStatus::Unavailable { .. }) {
        out.push("provider unavailable; do not route here".into());
        return out;
    }
    if s.freshness < 0.5 {
        out.push("data is stale or unavailable — treat capacity as a risk".into());
    }
    if !p.windows.is_empty() {
        let worst = p
            .windows
            .iter()
            .map(|w| w.value.used)
            .fold(0.0f64, f64::max);
        if worst < 0.25 {
            out.push(format!(
                "low usage across windows (worst {:.0}%)",
                worst * 100.0
            ));
        } else if worst > 0.8 {
            out.push(format!(
                "a window is near its limit ({:.0}% used)",
                worst * 100.0
            ));
        }
    }
    match &p.paid_overflow.value {
        Some(po) if po.enabled => out.push("paid overflow enabled".into()),
        _ => {}
    }
    if let Some(b) = p.api_balance.value.as_ref() {
        out.push(format!(
            "api balance healthy ({})",
            crate::view::format_money(&b.total)
        ));
    }
    if out.is_empty() {
        out.push("no strong signal either way".into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_task_kinds() {
        assert_eq!(TaskKind::parse("long-coding"), Some(TaskKind::LongCoding));
        assert_eq!(TaskKind::parse("nonsense"), None);
    }

    #[test]
    fn score_range_is_bounded() {
        // Property-ish: any report scores within [0,1].
        let p = crate::test_support::empty_report(ProviderId::OpenRouter);
        let s = score_one(&p, TaskKind::Audit);
        assert!((0.0..=1.0).contains(&s.score));
    }

    #[test]
    fn golden_ranking_low_usage_beats_high_usage() {
        // Codex at 10% session must outrank Claude at 95% session for long-coding,
        // even though Claude's task-fit for some tasks is high.
        let codex = crate::test_support::live_windowed_report(
            ProviderId::Codex,
            WindowKind::Session5h,
            0.10,
            None,
        );
        let claude = crate::test_support::live_windowed_report(
            ProviderId::Claude,
            WindowKind::Session5h,
            0.95,
            None,
        );
        let report = crate::report::AggregateReport::new(vec![codex, claude]);
        let recs = recommend(&report, TaskKind::LongCoding);
        assert_eq!(recs[0].provider, ProviderId::Codex);
        assert_eq!(recs[1].provider, ProviderId::Claude);
        assert!(recs[0].score > recs[1].score);
        // Every recommendation must carry at least one explainable reason.
        for r in &recs {
            assert!(!r.reasons.is_empty(), "no reasons for {:?}", r.provider);
        }
        // The high-usage Claude recommendation must flag the near-limit risk.
        assert!(claude_reason_flags_exhaustion(&recs[1].reasons));
    }

    fn claude_reason_flags_exhaustion(reasons: &[String]) -> bool {
        reasons
            .iter()
            .any(|r| r.contains("near its limit") || r.contains("90%"))
    }
}
