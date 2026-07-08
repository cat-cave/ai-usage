# ai-usage — Specification

> What AI providers do I have available right now, how constrained are they, what
> paid overflow exists, and which provider should an agent choose for this task?

`ai-usage` is an open-source, Nix-friendly command-line utility **and** library for
tracking AI coding-provider quotas, reset windows, balances, paid overflow, and
local usage velocity. It combines **live provider data**, **local usage history**,
and **account balance** into one honest, machine-readable view.

This document is the authoritative spec. Implementation tracks `ROADMAP.md`.

---

## 1. Design values (non-negotiable)

1. **Model provider reality precisely.** Do not flatten Codex/Claude/z.ai/MiniMax
   into a generic "credits" concept. Each provider keeps its own window shape.
2. **Never guess silently.** Every datum carries a `Source` and `Freshness`. If a
   value is unknown it is reported as `unavailable`, never as zero.
3. **Never print secrets.** Tokens, cookies, keys, and account IDs are redacted in
   every output path (human, JSON, logs, errors). See §10.
4. **Prefer explicit auth over scraping.** We reuse existing provider sessions
   (OAuth token files, API keys, CLI auth). The only "scraping" tolerated is the
   *optional* OpenAI web-dashboard enrichment, off by default and out of v1 scope.
5. **Make every data point traceable.** Each field records where it came from and
   when it was observed.
6. **Keep recommendations explainable.** Every recommendation emits human-readable
   reasons and machine-readable weights.
7. **Treat paid overflow as a first-class routing dimension.**
8. **Treat unavailable quota data as a risk, not as zero usage.**

---

## 2. Provider data sources (validated, real, non-scraping)

Every provider below has a legitimate data path. Endpoints confirmed against
CodexBar's published provider docs (MIT) and the local credential files present on
the target machine.

| Provider   | Live source (primary)                                                                                                              | Fallback / secondary                                       | Local velocity (ccusage-style)                      |
| ---------- | ---------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------- | --------------------------------------------------- |
| **codex**  | `GET https://chatgpt.com/backend-api/wham/usage` + `wham/rate-limit-reset-credits`, `Authorization: Bearer <access_token>` from `~/.codex/auth.json` (`tokens.access_token`, refresh via `refresh_token` when `last_refresh` > 8d) | `codex app-server` JSON-RPC: `initialize`, `account/read`, `account/rateLimits/read` | `~/.codex/sessions/**/*.jsonl` — `event_msg` `token_count` + `turn_context` model |
| **claude** | `GET https://api.anthropic.com/api/oauth/usage`, `Authorization: Bearer <access_token>` + `anthropic-beta: oauth-2025-04-20`, token from `~/.claude/.credentials.json` (`claudeAiOauth.accessToken`) | CLI PTY: spawn `claude --allowed-tools ""`, send `/usage`, parse rendered panel | `~/.claude/projects/**/*.jsonl` — `type:"assistant"` + `message.usage` per model |
| **z.ai**   | `GET https://api.z.ai/api/monitor/usage/quota/limit`, `authorization: Bearer <Z_AI_API_KEY>`; team scope adds `type=2` + `Bigmodel-Organization` / `Bigmodel-Project` headers | region override `open.bigmodel.cn` via `Z_AI_API_HOST`    | n/a (no local agent logs typically)                 |
| **minimax**| Coding-plan (`sk-cp-*`) subs have **no key-only quota endpoint** — `token_plan/remains` returns `invalid api key` and `coding_plan/remains` returns `cookie is missing` for a bearer. Requires a browser **session cookie** (file: `cookie_path` or `/run/secrets/minimax-cookie`): `GET https://<host>/v1/api/openplatform/coding_plan/remains` with `Cookie:` (+ optional `Authorization: Bearer <sk-cp-*>`). `sk-api-*` token-plan keys work key-only on `token_plan/remains`. Host default `api.minimax.chat`; `MINIMAX_HOST` must match the cookie's domain (e.g. `api.minimaxi.com`). | cookie file → `coding_plan/remains`; key → `token_plan/remains` then `coding_plan/remains` | n/a                                                 |
| **openrouter** | `GET https://openrouter.ai/api/v1/credits` (total_credits, total_usage) + `/api/v1/key` (limit + daily/weekly/monthly spend), `Authorization: Bearer <OPENROUTER_API_KEY>` | —                                                       | n/a                                                 |
| **deepseek**   | `GET https://api.deepseek.com/user/balance`, `Authorization: Bearer <DEEPSEEK_API_KEY>` → `balance_infos[]` (`total_balance`, `granted_balance`, `toped_up_balance`) | —                                                       | n/a                                                 |
| **grok**       | `POST https://grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig` (gRPC-web+proto, empty frame) with `Authorization: Bearer <key>` from `~/.grok/auth.json` (SuperGrok OIDC scope `https://auth.x.ai::*`). Response is a protobuf scanned for `used_percent` (fixed32, path ends in 1) and `resets_at` (varint Unix-ts, prefer path `[1,5,1]`); omitted percent + valid period ⇒ fresh cycle, 0%. | `grok agent stdio` JSON-RPC `x.ai/billing` (still `-32601` on stdio in grok 0.2.91; wired in TUI only) | `~/.grok/sessions/**/signals.json` — `contextTokensUsed` + `totalTokensBeforeCompaction` |

### Field → domain mapping

**Codex** `wham/usage`:
- `rate_limit.primary_window` → session window
- `rate_limit.secondary_window` → weekly window
- `additional_rate_limits[]` (e.g. `GPT-5.3-Codex-Spark`) → named model-family windows
- `wham/rate-limit-reset-credits` → banked reset credit inventory (expiry list; we list, never redeem)

**Claude** `api/oauth/usage`:
- `five_hour` → session window
- `seven_day` → weekly window (primary fallback when `five_hour` absent)
- `seven_day_sonnet` / `seven_day_opus` → model-family weekly windows
- `extra_usage` → paid overflow (monthly spend/limit)
- `subscriptionType` / `rate_limit_tier` → plan label (Max 5x / Max 20x multiplier surfaced)

**z.ai** `monitor/usage/quota/limit`:
- `data.limits[]` each with type/unit/number → window
  - `TOKENS_LIMIT` → primary tokens window
  - `TIME_LIMIT` → secondary MCP/time window
- `data.planName` → plan label
- `nextResetTime` (epoch ms) → reset
- `usageDetails[]` → per-model MCP usage

**OpenRouter**: `total_credits - total_usage` → balance; `/key` `limit` → primary meter; daily/weekly/monthly spend.

**DeepSeek**: per-currency `balance_infos`; USD preferred; granted vs topped-up split.

---

## 3. Domain model

Core types (Rust). Every quota/balance field is wrapped in `Sourced<T>` which pairs
the value with its provenance.

```rust
pub struct Sourced<T> {
    pub value: T,
    pub source: Source,        // where it came from
    pub observed_at: time::OffsetDateTime,
    pub ttl: Option<time::Duration>,  // when it goes stale
}

pub enum Source {
    LiveApi { endpoint: &'static str, status_code: Option<u16> },
    LocalLog { path: PathBuf },
    CliProbe { tool: &'static str },
    Cached,
    Config,      // explicit user/env config (auth identity, region, plan label only)
    Unavailable { reason: UnavailableReason },
}

pub enum Freshness { Live, Fresh, Stale, Unknown }

pub enum UnavailableReason {
    NoCredentials, NotConfigured, AuthFailed, Network, Parse,
    EndpointDisabled, ScopeMissing, UnknownEndpoint,
}
```

### Provider report

```rust
pub struct ProviderReport {
    pub id: ProviderId,                 // Codex|Claude|Zai|MiniMax|OpenRouter|DeepSeek
    pub account: Option<Sourced<AccountIdentity>>,  // email + plan label, never raw token
    pub windows: Vec<Sourced<WindowQuota>>,
    pub banked_resets: Vec<Sourced<BankedReset>>,
    pub plan_capacity: Option<Sourced<PlanCapacity>>,
    pub paid_overflow: Option<Sourced<PaidOverflow>>,
    pub api_balance: Option<Sourced<ApiBalance>>,
    pub local_velocity: Option<Sourced<LocalVelocity>>,
    pub status: ProviderStatus,         // Ok|Degraded|Unavailable (with reasons[])
}

pub struct WindowQuota {
    pub kind: WindowKind,               // Session5h|Hourly|Daily|Weekly|Monthly|ModelFamily(name)|Mcp|Coding
    pub used: Ratio,                    // 0.0..=1.0 (or counts)
    pub limit: Option<u64>,             // tokens | requests | minutes
    pub reset_at: Option<time::OffsetDateTime>,
    pub rolling: bool,
}

pub struct BankedReset { pub count: u32, pub expires_at: Option<...> }
pub struct PlanCapacity { pub included: u64, pub used: u64 }
pub struct PaidOverflow { pub enabled: bool, pub balance: Money, pub spent_this_cycle: Money }
pub struct ApiBalance { pub total: Money, pub granted: Option<Money>, pub paid: Option<Money> }
pub struct LocalVelocity { pub burn_rate_tokens_per_hour: f64, pub window: time::Duration,
                           pub cost_per_hour: Option<Money>, pub samples: u32 }
pub struct AccountIdentity { pub email_masked: Option<String>, pub plan: Option<String> }
```

### Money

`Money { amount: rust_decimal::Decimal, currency: Currency (Usd|Cny|...) }`. Rendered
per-locale; JSON as `{ "amount": "12.40", "currency": "USD" }`.

---

## 4. Source-tier resolution policy

For each provider, a `Resolver` picks the best available source and falls back
without ever silently inventing data:

```
codex:    LiveApi(wham)        -> CliProbe(app-server)   -> LocalLog(sessions) only
claude:   LiveApi(oauth/usage) -> CliProbe(claude /usage)-> LocalLog(projects) only
zai:      LiveApi(quota/limit) -> Unavailable
minimax:  LiveApi(coding_plan) -> Unavailable
openrouter: LiveApi(credits+key) -> Unavailable
deepseek:   LiveApi(balance)     -> Unavailable
```

A field that only has `LocalLog` provenance is reported as **velocity only** — the
quota windows are `Unavailable`, never inferred from spend. This satisfies design
value #8.

---

## 5. Recommendation engine

`ai-usage recommend --task <kind>` ranks providers.

**Task kinds:** `short` | `long-coding` | `exploratory` | `review` | `high-context` | `audit`.

**Per-provider score** in `[0,1]`, computed from weighted sub-scores:

| Sub-score        | Inputs                                                      | Notes |
| ---------------- | ---------------------------------------------------------- | ----- |
| `capacity`       | min over windows of `1 - used`, weighted by reset urgency | a window at 95% with reset in 2h scores worse than 95% resetting now |
| `velocity_fit`   | `local_velocity` vs remaining capacity / time-to-reset     | negative if burn exhausts before reset |
| `paid_ok`        | `paid_overflow.enabled` + balance + task paid-allowance flag | pure-API providers score neutral-positive here |
| `freshness`      | worst `Freshness` across fields used                       | `Unknown`/`Stale` caps the final score |
| `task_fit`       | task-kind heuristic vs provider archetype                  | review→Claude/Opus; long-coding→Codex; fallback→OpenRouter/DeepSeek |

**Output (explainable):**
```
recommend --task long-coding
  1. codex       0.81   capacity 0.90 · velocity 0.85 · fresh · fit long-coding
     → Prefer codex: low 5h usage, low weekly usage, current burn lasts through reset.
  2. openrouter  0.66   capacity 1.00 · paid fallback · balance $42.18 healthy
     → Use as paid fallback: no quota window, balance healthy.
  3. claude      0.41   capacity 0.45 · weekly healthy · session high
     → Reserve for targeted review; session usage high.
```

JSON mode emits `score`, `subscores`, `rank`, `reasons[]`, and the `task` kind. The
weights table is documented in `docs/recommender.md` and is config-overridable.

---

## 6. CLI surface

```
ai-usage                              # table, all configured providers
ai-usage --json                       # machine-readable aggregate
ai-usage provider <id> [--json]       # one provider deep view
ai-usage recommend --task <kind> [--json]
ai-usage config (show|validate|set-api-key --provider <id> --stdin)
ai-usage doctor                       # auth + freshness diagnostics (redacted)
ai-usage --offline                    # cache-only, no network
ai-usage --refresh                    # force live fetch, bypass cache
```

Default table matches the pitch (Provider / Session / Weekly / Paid Overflow /
Balance / Reset / Guidance), auto-compacting under 100 cols.

**Stability contract:** `--json` output follows a versioned JSON schema
(`docs/schema.md` + generated JSON Schema). Breaking changes bump
`schemaVersion`. `ai-usage` prints `schemaVersion` in every JSON payload.

---

## 7. Library API

The `ai-usage` crate is published for embedders (dashboards, orchestrators,
shell prompts):

```rust
use ai_usage::{Registry, TaskKind};
let report = Registry::from_env()?.snapshot().await?;
let rec = report.recommend(TaskKind::LongCoding);
```

- `Registry::snapshot()` returns `AggregateReport` (cheap, uses cache).
- `Provider` trait is public for adding custom providers downstream.
- No `println!` in the library; CLI owns rendering.

---

## 8. Configuration & secrets (sops-nix / NixOS native)

Secrets are **file-path based**, never environment values and never inline in
config. The config holds **paths** to secret files; the tool reads the file at
runtime, keeps the value in memory for the request, and never persists it.

- Config file: `~/.config/ai-usage/config.json` (XDG). Override with `AI_USAGE_CONFIG`.
  Holds only **paths** and non-secret knobs (host overrides, offline flag).
- Per-provider API key resolves in this order:
  1. `providers.<p>.api_key_path` (explicit path, e.g. `/run/secrets/openrouter-api-key`)
  2. conventional default `<secrets_root>/<p>-api-key` (default root `/run/secrets`)
  3. an assembled opencode-style `auth.json` at `~/.local/share/opencode/auth.json`
     (entry map: `openrouter`, `deepseek`, `minimax`, `zai-coding-plan`). On a
     sops-nix host that already renders this for opencode, every provider resolves
     with **zero** ai-usage config. Root-owned `/run/secrets/*` that exist but are
     unreadable are skipped, not fatal.
- **Zero-config on a sops-nix host**: name your secrets `<provider>-api-key`
  (`openrouter-api-key`, `deepseek-api-key`, `zai-api-key`, `minimax-api-key`)
  and they resolve automatically from `/run/secrets/`.
- Codex/Claude read their existing OAuth token **files** directly
  (`~/.codex/auth.json`, `~/.claude/.credentials.json`, honoring `CODEX_HOME` /
  `CLAUDE_CONFIG_DIR`) — the same file-path model, never env vars.
- `secrets_root` config knob overrides the `/run/secrets` default (tests,
  non-NixOS hosts).
- **No secret values are read from environment variables.** Env vars are used
  only for non-secret configuration (`AI_USAGE_OFFLINE`, `AI_USAGE_CONFIG`,
  `CODEX_HOME`, host overrides). Re-adding an opt-in env-value fallback is a
  deliberate, documented choice, not a default.
- The cache stores **results only**, never credentials.
- Endpoint overrides (`*_API_HOST`, `*_API_URL`) validated HTTPS-only before any
  bearer token is attached (fail closed).

---

## 9. Caching & freshness

- Cache: `~/.cache/ai-usage/snapshot.json` (+ per-provider shards), `0600`.
- Every cached value retains its `Source` + `observed_at` + `ttl`.
- Default TTLs: live-api 60s, local-log 30s, cli-probe 60s.
- `--offline` serves cache only and marks every field `Stale`/`Cached`.
- `--refresh` bypasses cache for one run.

---

## 10. Security & redaction

- A single `Redactor` wraps all output formatting and error paths. Redacts: API
  keys (`sk-*`, `sk-or-*`, `sk-ant-*`, `sk-cp-*`, `sk-api-*`), OAuth tokens
  (JWT-shaped strings), `refresh_token`, cookies, emails (masked to `t***@x.com`),
  account/org IDs.
- Tokens are read into memory, used for the request, and dropped; never written to
  logs or the cache. The cache stores **results only**, never credentials.
- `doctor` confirms auth works and reports freshness — all redacted.
- `gitleaks`/`typos` run in CI; a dedicated `test_redaction` golden asserts no
  secret pattern leaks through any output path.

---

## 11. Packaging & install

- Nix flake (this repo) with `flake-parts` + `crane` + `rust-overlay`, exposing:
  - `packages.default` — the `ai-usage` binary
  - `devShells.default` — pinned `cargo`, `rust-analyzer`, `just`, `rg`, `fd`,
    `jq`, `gh`, `typos`, `treefmt`, `gitleaks`
  - `checks` — `cargo test`, `clippy -D warnings`, `typos`, `treefmt --check`
- Install globally: `nix profile install .` (or `nix run github:<owner>/ai-usage`).
- Release binary: `opt-level="z"`, `lto="fat"`, `codegen-units=1`, `panic="abort"`,
  `strip="symbols"` (min-sized-rust).
- Downstream consumers (e.g. `nix-desktop`) add this repo as a flake input.

---

## 12. Testing & validation gates

| Layer        | Strategy                                                              |
| ------------ | -------------------------------------------------------------------- |
| Unit         | domain math, `Sourced` resolution, `Money`, window mapping           |
| Parser       | per-provider **recorded fixtures** (real response shapes, redacted)   |
| Recommender  | golden ranking cases per task kind; property test score ∈ [0,1]      |
| Contract     | HTTP layer via `wiremock`/recorded cassettes; never hit live in CI    |
| Redaction    | golden: feed canned secrets through every output, assert none leak   |
| CLI          | snapshot (`insta`) of table + JSON for fixed fixture reports         |
| Integration  | `nix build`, `nix flake check`, `nix profile install` in a sandbox    |

**Gate to call v1 "ready to install globally":**
1. `nix flake check` green; `cargo test` green; clippy clean.
2. ≥2 providers live against real APIs on this machine (OpenRouter + DeepSeek),
   Codex/Claude live where creds exist, others degrade to `Unavailable` cleanly.
3. `ai-usage --json` validates against the published JSON Schema.
4. Recommender golden cases pass; every recommendation has `reasons`.
5. `test_redaction` passes; `doctor` leaks nothing.
6. `nix profile install .` succeeds; `ai-usage --version` runs offline.

---

## 13. Explicitly out of scope for v1

- OpenAI web-dashboard WebView scraping (Codex extras) — future, opt-in only.
- Browser cookie import (Safari/Chrome/Firefox) — future; v1 uses API tokens +
  OAuth token files + CLI PTY only.
- Account switching UI (claude-swap integration) — future.
- Provider status/incident polling — future.
