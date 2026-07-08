# ai-usage — Roadmap

Build order is phase-gated. Each phase is independently shippable and ends with a
green `just check` + `just test`. Do not start a phase until the previous phase's
validation gate passes.

## Phase 0 — Foundation & install path ✅ (this checkpoint)
- Repo meta: `AGENTS.md`, `README.md`, `SPEC.md`, `.editorconfig`, `.envrc`,
  `.gitignore`, `rust-toolchain.toml`, `justfile`.
- Cargo workspace: `crates/ai-usage` (lib) + `crates/ai-usage-cli` (bin).
- Domain model: `Sourced<T>`, `Source`, `Freshness`, `ProviderReport`, `WindowQuota`,
  `Money`, `Ratio`, `ProviderStatus`, `ProviderId`.
- `Provider` trait + `Registry` + `Config` + `Secret` (env / `_FILE` / config).
- Nix flake (`flake-parts` + `crane` + `rust-overlay`): `packages.default`,
  `devShells.default`, `checks`.
- CLI skeleton: table renderer + `--json` + `provider <id>` + `recommend` + `doctor`
  + `--offline`/`--refresh` (wired through the registry, all providers Unavailable
  except the implemented ones).
- **Gate:** `nix build .` succeeds; `nix profile install .` works; `ai-usage --version`
  runs offline; table+JSON emit `schemaVersion` and degrade cleanly with no providers.

## Phase 1 — First real provider (OpenRouter) ✅ reference vertical
- Implement `OpenRouterProvider`: `/api/v1/credits` + `/api/v1/key`, bearer from
  `OPENROUTER_API_KEY` / `_FILE` / config.
- Map → `ApiBalance` + `PaidOverflow` (pay-as-you-go) + primary meter window.
- Recorded-fixture parser tests; `wiremock` contract test.
- Redaction golden for `sk-or-*`.
- **Gate:** `ai-usage provider openrouter --json` returns live data on this machine
  when a key is present, else clean `Unavailable{NoCredentials}`.

## Phase 2 — Remaining pure-API providers
- `DeepSeekProvider` (`/user/balance` → granted vs paid).
- Both reuse the Phase 1 HTTP + redaction infrastructure.
- **Gate:** two live providers; `recommend --task audit` prefers the healthier
  balance with reasons.

## Phase 3 — Codex (headline windowed provider)
- OAuth token load from `~/.codex/auth.json`; `last_refresh` > 8d → refresh via
  `refresh_token`.
- `GET chatgpt.com/backend-api/wham/usage` + `wham/rate-limit-reset-credits`.
- Map primary/secondary/additional windows; banked resets listed, never redeemed.
- Local velocity from `~/.codex/sessions/**/*.jsonl` (`event_msg.token_count` +
  `turn_context` model).
- `codex app-server` JSON-RPC fallback.
- **Gate:** `ai-usage provider codex` shows real session/weekly + reset + velocity.

## Phase 4 — Claude
- OAuth token from `~/.claude/.credentials.json` (`claudeAiOauth.accessToken`); if
  absent (Claude Code 2.1.x mcpOAuth-only) → `CliProbe` via `claude /usage` PTY.
- `GET api.anthropic.com/api/oauth/usage` (`anthropic-beta: oauth-2025-04-20`).
- Map five_hour/seven_day/sonnet/opus/extra_usage.
- Local velocity from `~/.claude/projects/**/*.jsonl` (`type:"assistant"` + usage).
- **Gate:** `ai-usage provider claude` shows real windows + paid overflow.

## Phase 5 — z.ai + MiniMax
- `ZaiProvider`: `quota/limit`, personal + team scope (Bigmodel headers).
- `MiniMaxProvider`: coding-plan `remains` (`sk-cp-*` > `sk-api-*`), region host.
- **Gate:** all six providers live or cleanly Unavailable; full pitch example table
  renders against real data.

## Phase 6 — Recommender hardening
- Finalize sub-score weights; add per-task-kind config overrides.
- Velocity projection: warn when current burn exhausts before reset.
- Explainability: every recommendation emits 1–3 human reasons + machine weights.
- Golden ranking suites per task kind; property test for score range.
- **Gate:** golden cases pass; `recommend` reasons match pitch examples.

## Phase 7 — Polish & release
- JSON Schema generation + `docs/schema.md`; sample tables in README.
- `treefmt` + `gitleaks` + `typos` in `checks`; CI workflow.
- `CHANGELOG.md`; tag `v0.1.0`; cut GitHub release with Nix flake URL.
- Wire as flake input into `nix-desktop` (separate PR, downstream).
- **Gate:** all §12 gates green; `nix profile install .` from a clean clone works.
