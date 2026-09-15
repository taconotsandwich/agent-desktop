#!/usr/bin/env bash
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
cargo build --release --locked --manifest-path "$REPO/Cargo.toml"
exec "$REPO/target/release/agent-desktop" setup --bin-dir "${PREFIX:-$HOME/.local}/bin"
