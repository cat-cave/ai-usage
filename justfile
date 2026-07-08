# ai-usage — task entry points. Run inside the Nix dev shell (`direnv allow`).

# Rust workspace is under ./rust
RUST_DIR := "rust"

fmt:
    cd {{RUST_DIR}} && cargo fmt
    @command -v treefmt >/dev/null 2>&1 && treefmt || true

check:
    cd {{RUST_DIR}} && cargo fmt --check
    cd {{RUST_DIR}} && cargo clippy --all-targets -- -D warnings
    @command -v typos >/dev/null 2>&1 && typos || true

test:
    cd {{RUST_DIR}} && cargo test --all

# Supply-chain: license/advisory/ban/source checks via cargo-deny.
deny:
    cd {{RUST_DIR}} && cargo deny check

# Vulnerability advisory scan (RustSec) via cargo-audit.
audit:
    cd {{RUST_DIR}} && cargo audit

build:
    cd {{RUST_DIR}} && cargo build --release --package ai-usage-cli

run *ARGS:
    cd {{RUST_DIR}} && cargo run --package ai-usage-cli -- {{ARGS}}

nix-build:
    nix build . --print-build-logs

nix-run *ARGS:
    nix run . -- {{ARGS}}

nix-install:
    nix profile install .

nix-check:
    nix flake check --print-build-logs

lock:
    cd {{RUST_DIR}} && cargo update --workspace

# Generate a redacted fixture from a live provider response (phase 1+ helper).
# Usage: just capture openrouter
capture provider:
    cd {{RUST_DIR}} && cargo run --package ai-usage-cli -- doctor
