#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- --deny warnings
cargo test --workspace --all-features
cargo doc --workspace --no-deps --all-features
cargo run --example loopback -p mokmok-event-bus

# Only `nix flake check` runs treefmt, which is what holds `Cargo.toml` to taplo's wrapping and
# `flake.nix` to nixfmt's. The steps above cannot see a violation there.
if command -v nix >/dev/null 2>&1; then
  nix flake check
else
  echo "nix is not on PATH: run 'nix flake check' before pushing, CI requires it" >&2
fi
