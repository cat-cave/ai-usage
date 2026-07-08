//! CLI rendering. This crate owns all terminal output; the library is side-effect free.

use ai_usage::model::{ProviderId, ProviderReport, ProviderStatus, WindowKind};
use ai_usage::recommend::{Recommendation, TaskKind};
use ai_usage::report::AggregateReport;
use ai_usage::source::Source;
use ai_usage::view::format_money;

pub fn render_table(report: &AggregateReport) -> String {
    let header = format!(
        "{:<11} {:<12} {:<12} {:<18} {:<13} {:<11} {}",
        "Provider", "Session", "Weekly", "Paid Overflow", "Balance", "Reset", "Guidance"
    );
    let mut out = String::from(&header);
    out.push('\n');
    out.push_str(&"-".repeat(header.len()));
    out.push('\n');
    for p in &report.providers {
        out.push_str(&render_row(p));
        out.push('\n');
    }
    out
}

fn render_row(p: &ProviderReport) -> String {
    let name = display_name(p.id);
    // Session column: prefer an actual session window; fall back to the
    // provider's primary window (z.ai TOKENS_LIMIT/Coding, MiniMax Coding, …)
    // so providers without 5h/weekly windows still surface their usage %.
    let session = window_pct(
        p,
        &[
            WindowKind::Session5h,
            WindowKind::Coding,
            WindowKind::Hourly,
            WindowKind::Daily,
        ],
    );
    let weekly = window_pct(
        p,
        &[WindowKind::Weekly, WindowKind::Mcp, WindowKind::Monthly],
    );
    let paid = paid_cell(p);
    let balance = balance_cell(p);
    let reset = reset_cell(p);
    let guidance = guidance_cell(p);
    format!("{name:<11} {session:<12} {weekly:<12} {paid:<18} {balance:<13} {reset:<11} {guidance}")
}

const fn display_name(id: ProviderId) -> &'static str {
    match id {
        ProviderId::Codex => "Codex",
        ProviderId::Claude => "Claude",
        ProviderId::Zai => "z.ai",
        ProviderId::MiniMax => "MiniMax",
        ProviderId::OpenRouter => "OpenRouter",
        ProviderId::DeepSeek => "DeepSeek",
        ProviderId::Grok => "Grok",
    }
}

/// Find the first window (in priority order) matching one of `kinds` and render
/// its used %. Lets a provider's primary window (e.g. z.ai TOKENS_LIMIT) populate
/// the Session column when it has no true 5h-session window.
fn window_pct(p: &ProviderReport, kinds: &[WindowKind]) -> String {
    for k in kinds {
        if let Some(w) = p.windows.iter().find(|w| &w.value.kind == k) {
            return format!("{:.0}% used", w.value.pct_used());
        }
    }
    "n/a".into()
}

fn paid_cell(p: &ProviderReport) -> String {
    match &p.paid_overflow.source {
        Source::Unavailable { .. } => match &p.api_balance.value {
            Some(_) => "pay-as-you-go".to_string(),
            None => "n/a".into(),
        },
        _ => match &p.paid_overflow.value {
            Some(po) if po.enabled => "enabled".into(),
            Some(_) => "disabled".into(),
            None => "n/a".into(),
        },
    }
}

fn balance_cell(p: &ProviderReport) -> String {
    p.api_balance
        .value
        .as_ref()
        .map_or_else(|| "n/a".into(), |b| format_money(&b.total))
}

fn reset_cell(p: &ProviderReport) -> String {
    p.windows
        .iter()
        .find_map(|w| w.value.reset_at)
        .map_or_else(|| "n/a".into(), format_reset)
}

/// Relative reset: within ~18h show the clock time (imminent), otherwise show
/// the date so a weekly reset isn't mistaken for later today.
fn format_reset(t: time::OffsetDateTime) -> String {
    let now = time::OffsetDateTime::now_utc();
    let delta = t - now;
    if delta < time::Duration::ZERO {
        return "now".into();
    }
    if delta < time::Duration::hours(18) {
        let local = to_local(t).unwrap_or(t);
        format!("{}:{:02}", local.hour(), local.minute())
    } else {
        let d = to_local(t).unwrap_or(t);
        let month = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        format!(
            "{} {}",
            month.get(d.month() as usize - 1).unwrap_or(&""),
            d.day()
        )
    }
}

const fn to_local(t: time::OffsetDateTime) -> Option<time::OffsetDateTime> {
    // time 0.3 does not read TZ without the `local-offset` feature; fall back to UTC.
    // Kept as a hook for a future render-time formatting pass.
    let _ = t;
    None
}

fn guidance_cell(p: &ProviderReport) -> String {
    match &p.status {
        ProviderStatus::Ok => {
            if let Some(b) = &p.api_balance.value {
                if matches!(&p.paid_overflow.value, Some(po) if po.enabled) {
                    return "good".into();
                }
                return format!("balance {}", format_money(&b.total));
            }
            "ok".into()
        }
        ProviderStatus::Degraded { .. } => "degraded".into(),
        ProviderStatus::Unavailable { .. } => "unavailable".into(),
    }
}

pub fn render_json(report: &AggregateReport) -> anyhow::Result<String> {
    // Defense in depth: even if a provider accidentally stored a raw token or
    // unmasked PII, the redactor scrubs it before it reaches the terminal.
    let raw = serde_json::to_string_pretty(report)?;
    Ok(ai_usage::redact::Redactor::redact_str(&raw))
}

pub fn render_recommendations(recs: &[Recommendation], task: TaskKind) -> String {
    use std::fmt::Write;
    let mut out = format!("recommend --task {}\n\n", task.kebab());
    for r in recs {
        let _ = writeln!(
            out,
            "{:>2}. {:<11} {:.2}",
            r.rank,
            display_name(r.provider),
            r.score
        );
        let _ = writeln!(
            out,
            "     capacity {:.2} · velocity {:.2} · paid {:.2} · fresh {:.2} · fit {:.2}",
            r.subscores.capacity,
            r.subscores.velocity_fit,
            r.subscores.paid_ok,
            r.subscores.freshness,
            r.subscores.task_fit,
        );
        for reason in &r.reasons {
            let _ = writeln!(out, "     → {reason}");
        }
        out.push('\n');
    }
    out
}

pub fn render_doctor(report: &AggregateReport) -> String {
    use std::fmt::Write;
    let mut out = String::from("ai-usage doctor (all values redacted)\n\n");
    for p in &report.providers {
        let _ = writeln!(out, "{:<11} {:?}", display_name(p.id), p.status);
        let _ = writeln!(out, "  account     {}", src_summary(&p.account.source));
        let _ = writeln!(out, "  balance     {}", src_summary(&p.api_balance.source));
        let _ = writeln!(
            out,
            "  velocity    {}",
            src_summary(&p.local_velocity.source)
        );
    }
    out
}

fn src_summary(s: &Source) -> String {
    match s {
        Source::LiveApi {
            endpoint,
            status_code,
        } => {
            format!("live-api:{endpoint} (status {status_code:?})")
        }
        Source::LocalLog { path } => format!("local-log: {}", path.display()),
        Source::CliProbe { tool } => format!("cli-probe: {tool}"),
        Source::Cached => "cached".into(),
        Source::Config => "config".into(),
        Source::Unavailable { reason } => format!("unavailable: {reason:?}"),
    }
}
