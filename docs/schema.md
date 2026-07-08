# JSON output schema — `ai-usage --json`

`ai-usage --json` (and `ai-usage provider <id> --json`) emit an
`AggregateReport`. The payload is **versioned**: every response includes
`schemaVersion`. Breaking changes bump the number; consumers MUST check it.

- **Current schemaVersion: `1`** (unstable until v0.1.0 is tagged, then frozen).
- Stability contract: additive changes (new optional fields, new enum variants
  marked optional) do NOT bump the version. Removed fields or changed shapes DO.

## Top-level

```jsonc
{
  "schemaVersion": 1,
  "providers": [ ProviderReport ]
}
```

## ProviderReport

```jsonc
{
  "id": "codex" | "claude" | "zai" | "minimax" | "openrouter" | "deepseek" | "grok",
  "account":     Sourced<AccountIdentity | null>,
  "windows":     [ Sourced<WindowQuota> ],
  "banked_resets": [ Sourced<BankedReset> ],
  "plan_capacity":  Sourced<PlanCapacity | null>,
  "paid_overflow":  Sourced<PaidOverflow | null>,
  "api_balance":    Sourced<ApiBalance | null>,
  "local_velocity": Sourced<LocalVelocity | null>,
  "status": "ok" | { "degraded": { "notes": [string] } } | { "unavailable": { "reason": string } }
}
```

## Sourced\<T\> — every datum is traceable

```jsonc
{
  "value": T | null,                 // null when the datum is unavailable
  "source": Source,                  // where it came from (see below)
  "observed_at": "2026-07-08T16:00:00Z",   // RFC3339
  "ttl": [seconds, nanos] | null
}
```

A `null` value with `source.kind = "unavailable"` is the canonical "we don't
know" signal. **Never** interpret a missing/null field as zero — it is a risk to
surface, not a number to compute on.

## Source

```jsonc
{ "kind": "live_api",   "endpoint": "codex.wham-usage", "status_code": 200 }
{ "kind": "local_log",  "path": "/home/trevor/.codex/sessions" }
{ "kind": "cli_probe",  "tool": "claude /usage" }
{ "kind": "cached" }
{ "kind": "config" }
{ "kind": "unavailable", "reason": { "kind": "no_credentials" } }
```

`UnavailableReason.kind`: `no_credentials` | `not_configured` | `auth_failed` |
`network` | `parse` | `endpoint_disabled` | `scope_missing` |
`unknown_endpoint` | `disabled`.

## WindowQuota

```jsonc
{
  "kind": "session_5h" | "hourly" | "daily" | "weekly" | "monthly"
        | "model_family" | "mcp" | "coding",
  "label": string | null,            // e.g. "GPT-5.3-Codex-Spark", "opus"
  "used": 0.13,                       // 0.0–1.0
  "limit": u64 | null,                // tokens | requests | minutes
  "used_count": u64 | null,
  "reset_at": "2026-07-08T19:36:00Z" | null,
  "rolling": bool
}
```

## Money / balance shapes

```jsonc
Money = { "amount": "42.40", "currency": "USD" }   // Decimal as string; USD|CNY|EUR|UNKNOWN
ApiBalance  = { "total": Money, "granted": Money|null, "paid": Money|null }
PaidOverflow = { "enabled": bool, "balance": Money|null, "spent_this_cycle": Money|null }
```

## Recommendation (`ai-usage recommend --json`)

Emits an array of:

```jsonc
{
  "rank": 1,
  "provider": "codex",
  "score": 0.69,                      // 0.0–1.0, higher is better
  "subscores": {
    "capacity": 0.87, "velocity_fit": 0.70, "paid_ok": 0.30,
    "freshness": 0.90, "task_fit": 0.95
  },
  "reasons": [ "low usage across windows (worst 13%)", ... ]
}
```

## Redaction guarantee

Every `--json` output is passed through the `Redactor` as defense-in-depth: API
keys (`sk-*`), OAuth/JWT tokens, `refresh_token`/`access_token` JSON fields, and
raw emails are scrubbed to `[REDACTED]`. Account emails are masked to
`t***@domain` at the source (which the redactor preserves). No credential value
ever appears in `--json` output.
