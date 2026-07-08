# AGENTS.md

This file orients agents working in `ai-usage`. Read `SPEC.md` for the contract and
`ROADMAP.md` for phase gates before making changes.

## What this project is

`ai-usage` is a Rust CLI + library that reports AI coding-provider capacity
(quotas, reset windows, balances, paid overflow, local velocity) and recommends a
provider for a given task. The headline rule: **every datum is traceable to a
source, and unavailable data is reported as a risk — never as zero.**

## Layout

- `rust/crates/ai-usage` — the library: domain model, providers, recommender,
  config, cache, redaction. New runtime behavior goes here.
- `rust/crates/ai-usage-cli` — the `ai-usage` binary. Owns rendering only; no
  business logic.
- `nix/` — flake modules (packages, devShell, checks).
- `SPEC.md` — authoritative spec. `ROADMAP.md` — phased build.
- `docs/` — provider notes, recommender weights, JSON schema (generated).

## First-run setup

```
direnv allow        # loads pinned Nix dev shell (cargo, just, rg, fd, jq, gh, typos, treefmt, gitleaks)
just --list         # every task entry point
```

## Conventions

- **Provenance is mandatory.** No quota/balance value leaves a provider adapter
  without being wrapped in `Sourced<T>` (value + `Source` + `observed_at` + `ttl`).
  `Unavailable` is a valid value; `0` used to mean "unknown" is a bug.
- **Library is side-effect free.** No `println!`/`eprintln!` in `crates/ai-usage`.
  The CLI crate owns all rendering. Library errors use `thiserror`; fallible IO in
  glue uses `anyhow`.
- **Async at the edges.** `Provider::fetch` is `async fn` (tokio + reqwest/rustls).
  Domain math is sync and pure, so it tests without a runtime.
- **Never print secrets.** Route every string that may touch a credential through
  `Redactor`. The `test_redaction` golden feeds canned secrets through every output
  path; a leak fails CI.
- **Modules stay small.** Narrow `pub(crate)`; one concern per file. Prefer
  fixture-backed parser tests over live network.
- **HTTPS-only endpoint overrides.** `_API_HOST`/`_API_URL` overrides must be HTTPS
  or they fail closed before any bearer token is attached.

## Checks (run through `just`)

```
just fmt         # rustfmt + treefmt
just check       # cargo fmt --check + clippy -D warnings + typos
just test        # cargo test (unit + parser fixtures + recommender goldens + redaction)
just nix-check   # nix flake check
```

Run `just check` after every change that touches behavior. Rely on `just test` to
cover parser/recommender/redaction regressions.

## Adding a provider

1. `crates/ai-usage/src/providers/<name>.rs` implementing `Provider`.
2. Add the live source per `SPEC.md §2`; never scrape. Reuse `Redactor`.
3. Recorded-fixture parser test under `tests/fixtures/<provider>/` (redacted real
  response shapes). `wiremock` contract test for the HTTP path.
4. Register in `Registry::from_env` + `ProviderId`.
5. Update `SPEC.md` field-mapping table if new fields appear.
6. Update `docs/providers.md`.

## Commit / PR

- Small, revertable commits; squash-merge by default. No `--amend` unless asked.
- US English for all repo-facing text.
- Do not proactively add docs beyond what a change requires; do not commit secrets.
