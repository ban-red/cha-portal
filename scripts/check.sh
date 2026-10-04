#!/usr/bin/env sh
# Everything CI would run: Rust formatting, lints and tests across the
# workspace, and the portal's typecheck and production build.
set -eu
cd "$(dirname "$0")/.."

echo "==> cargo fmt"
cargo fmt --all --check
echo "==> cargo clippy"
cargo clippy --workspace --all-targets -- -D warnings
echo "==> cargo test"
cargo test --workspace --quiet
echo "==> portal: typecheck + build"
bun run --cwd web/apps/portal build >/dev/null
echo "All checks passed."
