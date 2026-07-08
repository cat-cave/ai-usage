//! Domain model. See `SPEC.md §3`.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::source::{OptionalSourced, Sourced};

/// The six first-class providers. Each keeps its own window shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderId {
    Codex,
    Claude,
    Zai,
    MiniMax,
    OpenRouter,
    DeepSeek,
    Grok,
}

impl ProviderId {
    pub const ALL: [Self; 7] = [
        Self::Codex,
        Self::Claude,
        Self::Zai,
        Self::MiniMax,
        Self::OpenRouter,
        Self::DeepSeek,
        Self::Grok,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Zai => "zai",
            Self::MiniMax => "minimax",
            Self::OpenRouter => "openrouter",
            Self::DeepSeek => "deepseek",
            Self::Grok => "grok",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountIdentity {
    /// Always masked (`t***@x.com`); never the raw email.
    pub email_masked: Option<String>,
    pub plan: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    Session5h,
    Hourly,
    Daily,
    Weekly,
    Monthly,
    /// Model-specific limit, e.g. "Codex Spark" or Claude "Sonnet"/"Opus" weekly.
    ModelFamily,
    /// MCP/time window (z.ai).
    Mcp,
    /// Coding-plan window (MiniMax).
    Coding,
}

/// A ratio plus optional concrete counts. `used` is always in `0.0..=1.0`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowQuota {
    pub kind: WindowKind,
    pub label: Option<String>,
    pub used: f64,
    pub limit: Option<u64>,
    pub used_count: Option<u64>,
    pub reset_at: Option<OffsetDateTime>,
    pub rolling: bool,
}

impl WindowQuota {
    pub fn pct_used(&self) -> f64 {
        self.used.clamp(0.0, 1.0) * 100.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BankedReset {
    pub count: u32,
    pub expires_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanCapacity {
    pub included: u64,
    pub used: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Currency {
    Usd,
    Cny,
    Eur,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Money {
    pub amount: Decimal,
    pub currency: Currency,
}

impl Money {
    pub fn usd(amount: impl Into<Decimal>) -> Self {
        Self {
            amount: amount.into(),
            currency: Currency::Usd,
        }
    }
    pub fn cny(amount: impl Into<Decimal>) -> Self {
        Self {
            amount: amount.into(),
            currency: Currency::Cny,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaidOverflow {
    pub enabled: bool,
    pub balance: Option<Money>,
    pub spent_this_cycle: Option<Money>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiBalance {
    pub total: Money,
    pub granted: Option<Money>,
    pub paid: Option<Money>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalVelocity {
    pub burn_rate_tokens_per_hour: f64,
    pub window_hours: u64,
    pub cost_per_hour: Option<Money>,
    pub samples: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatus {
    /// All expected fields resolved from a live source.
    Ok,
    /// Some fields unavailable; report still usable.
    Degraded { notes: Vec<String> },
    /// The provider could not be reached at all.
    Unavailable { reason: String },
}

/// A full report for one provider. Every optional field carries provenance;
/// absence is `Unavailable{reason}`, never silent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderReport {
    pub id: ProviderId,
    pub account: OptionalSourced<AccountIdentity>,
    pub windows: Vec<Sourced<WindowQuota>>,
    pub banked_resets: Vec<Sourced<BankedReset>>,
    pub plan_capacity: OptionalSourced<PlanCapacity>,
    pub paid_overflow: OptionalSourced<PaidOverflow>,
    pub api_balance: OptionalSourced<ApiBalance>,
    pub local_velocity: OptionalSourced<LocalVelocity>,
    pub status: ProviderStatus,
}
