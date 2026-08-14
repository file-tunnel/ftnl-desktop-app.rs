# shellcheck shell=bash
set -euo pipefail

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/.cache/nix-agent/cargo-target}"
export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}"

tlc -workers 1 -metadir .cache/nix-agent/tlc formal/DesktopLifecycle.tla \
  -config formal/DesktopLifecycle.cfg

cargo fmt --all -- --check
cargo clippy --no-default-features --all-targets --locked -- -D warnings
cargo test --no-default-features --all-targets --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
python3 scripts/validate-dependencies.py
actionlint
shellcheck .nix/agent-check.sh
shfmt -d -i 2 -ci .nix/agent-check.sh
