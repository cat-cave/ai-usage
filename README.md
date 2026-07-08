# ai-usage

> What AI providers do I have available right now, how constrained are they, what
> paid overflow exists, and which provider should an agent choose for this task?

`ai-usage` is an open-source, Nix-friendly command-line utility and Rust library
for tracking AI coding-provider quotas, reset windows, balances, paid overflow, and
local usage velocity. It combines **live provider data**, **local usage history**,
and **account balances** into one honest, machine-readable view — and recommends a
provider for a given task.

Every datum is traceable to a source. Unavailable data is reported as a **risk**,
never as zero.

## Status

Pre-release. See `SPEC.md` (contract) and `ROADMAP.md` (phase gates).

Supported providers (planned): `codex`, `claude`, `zai`, `minimax`, `openrouter`,
`deepseek`. Each models its real window shape rather than flattening to "credits".

## Install (Nix)

```bash
nix profile install .          # global install from this checkout
nix run . -- --version         # one-off run
```

Downstream flakes add this repo as a flake input.

## Usage

```bash
ai-usage                                 # human table, all configured providers
ai-usage --json                          # machine-readable aggregate (versioned schema)
ai-usage provider codex                  # one provider, deep view
ai-usage recommend --task long-coding    # ranked, explainable recommendation
ai-usage doctor                          # auth + freshness diagnostics (redacted)
ai-usage config set-api-key --provider openrouter --stdin
```

Example output:

```
Provider    Session      Weekly       Paid Overflow      Balance       Reset       Guidance
Codex       2% used      7% used      enabled            $12.40        7:36 PM     prefer
Claude      61% used     44% used     enabled            $8.15         8:12 PM     reserve
z.ai        18% used     OK           disabled           n/a           6:00 PM     good overflow
MiniMax     5% used      OK           disabled           n/a           9:00 PM     good
OpenRouter  n/a          n/a          pay-as-you-go      $42.18        n/a         paid fallback
DeepSeek    n/a          n/a          pay-as-you-go      ¥128.40       n/a         paid fallback
```

## Secrets

Secrets are **file-path based** (sops-nix / NixOS native), never environment
values and never inline in config. Name your sops-nix secrets
`<provider>-api-key` (`openrouter-api-key`, `deepseek-api-key`, `zai-api-key`,
`minimax-api-key`) and they resolve automatically from `/run/secrets/`. Codex and
Claude reuse their existing OAuth token files (`~/.codex/auth.json`,
`~/.claude/.credentials.json`). The tool never stores tokens in its cache and never
prints them. See `SPEC.md §8` and `§10`.

## Develop

```bash
direnv allow
just --list        # fmt | check | test | nix-check | build
```

## License

MIT.
