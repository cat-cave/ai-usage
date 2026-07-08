# Installing ai-usage

> **TL;DR for agents / humans on NixOS:**
>
> ```bash
> nix profile install github:cat-cave/ai-usage
> ```
>
> Or, declaratively in a NixOS flake — add this repo as an input and enable the
> module (see [§Declarative (NixOS module)](#declarative-nixos-module)).

`ai-usage` is a Nix flake. It works on any Linux/macOS machine with Nix (flakes
enabled). No cargo, no rustup, no system dependencies — the flake pins the
toolchain and builds a static-ish binary with `rustls` (no OpenSSL).

## 1. One-off run (no install)

```bash
nix run github:cat-cave/ai-usage -- --version
nix run github:cat-cave/ai-usage -- recommend --task long-coding
```

## 2. Install globally (imperative, user profile)

```bash
nix profile install github:cat-cave/ai-usage
ai-usage            # now on PATH
```

Upgrade: `nix profile upgrade '.*ai-usage.*'`. Remove: `nix profile remove ai-usage`.

## 3. Declarative (NixOS module) — recommended for hosts like `nix-desktop`

Add the input and the module:

```nix
# flake.nix
inputs.ai-usage.url = "github:cat-cave/ai-usage";
```

Then in your host config:

```nix
{ inputs, pkgs, system, ... }:
{
  # Option A — the module (provides programs.ai-usage.enable):
  imports = [ inputs.ai-usage.nixosModules.default ];
  programs.ai-usage.enable = true;

  # Option B — or just the package directly:
  # environment.systemPackages = [ inputs.ai-usage.packages.${system}.default ];
}
```

For **home-manager**:

```nix
imports = [ inputs.ai-usage.homeModules.default ];
programs.ai-usage.enable = true;
```

There is also a default overlay (`inputs.ai-usage.overlays.default`) exposing
`pkgs.ai-usage` if you prefer that style.

## 4. Secrets (read at runtime, file-path based)

`ai-usage` never reads secret values from env vars or stores them in config — it
reads **files**. On a sops-nix host, name secrets so they resolve automatically:

| provider    | sops secret name      |
| ----------- | --------------------- |
| codex       | `~/.codex/auth.json` (OAuth, already present) |
| claude      | `~/.claude/.credentials.json` (OAuth, already present) |
| openrouter  | `openrouter-api-key`  |
| deepseek    | `deepseek-api-key`    |
| z.ai        | `zai-coding-plan-key` (also auto-reads `~/.local/share/opencode/auth.json`) |
| minimax     | `minimax-api-key` (+ `minimax-cookie` for coding-plan quota) |
| grok        | `~/.grok/auth.json` (SuperGrok OAuth, already present) |

`sops-nix` renders these to `/run/secrets/`; `ai-usage` reads them from there
with zero extra config. See `SPEC.md §8` for the full resolution order.

## 5. Verify

```bash
ai-usage                    # table of all configured providers
ai-usage doctor             # auth + freshness diagnostics (redacted)
ai-usage recommend --task long-coding --json
```

## 6. Non-Nix

```bash
cd rust && cargo install --path crates/ai-usage-cli
```

A `rustup`-managed stable toolchain is required (`rust-toolchain.toml` pins it
for Nix and cargo).
