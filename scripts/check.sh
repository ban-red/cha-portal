#!/usr/bin/env sh
# Everything CI would run: Rust formatting, lints and tests across the
# workspace, the Steam image's scripts' tests (the status watcher, the library
# registration), the node's host installer's tests, and the portal's colour guard,
# theme contrast tests, typecheck and production build.
set -eu
cd "$(dirname "$0")/.."

echo "==> cargo fmt"
cargo fmt --all --check
echo "==> cargo clippy"
cargo clippy --workspace --all-targets -- -D warnings
echo "==> cargo test"
cargo test --workspace --quiet
echo "==> steam image scripts tests"
python3 -m unittest discover -s images/steam
echo "==> node host installer tests"
python3 -m unittest discover -s deploy/node/host
echo "==> portal: raw-colour guard + theme contrast tests"
sh scripts/check-portal-colors.sh
bun run --cwd web/apps/portal test
echo "==> portal: typecheck + build"
bun run --cwd web/apps/portal build >/dev/null
echo "All checks passed."
