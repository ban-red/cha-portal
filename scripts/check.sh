#!/usr/bin/env sh
# Everything CI would run: Rust formatting, the workspace-hack's freshness,
# lints and tests across the workspace, the Steam image's scripts' tests (the
# status watcher, the library registration), the node's host installer's
# tests, and the portal's colour guard, theme contrast tests, the browser
# player's tests (including the shared health cases), the player themes'
# freshness, the UI spec's check, typecheck and production build.
set -eu
cd "$(dirname "$0")/.."

echo "==> cargo fmt"
cargo fmt --all --check
echo "==> cargo hakari (crates/cha-workspace-hack up to date)"
if ! command -v cargo-hakari >/dev/null 2>&1; then
  echo "cargo-hakari is missing: cargo install cargo-hakari --locked" >&2
  exit 1
fi
cargo hakari generate --diff >/dev/null || {
  echo "the workspace-hack is stale: cargo hakari generate && cargo hakari manage-deps" >&2
  exit 1
}
cargo hakari manage-deps --dry-run >/dev/null || {
  echo "a crate lacks the workspace-hack dependency: cargo hakari manage-deps" >&2
  exit 1
}
echo "==> cargo clippy"
cargo clippy --workspace --all-targets -- -D warnings
echo "==> cargo test"
cargo test --workspace --quiet
echo "==> steam image scripts tests"
python3 -m unittest discover -s images/steam
echo "==> kde image scripts tests"
python3 -m unittest discover -s images/kde
echo "==> node host installer tests"
python3 -m unittest discover -s deploy/node/host
echo "==> image inputs (every image's crates still exist)"
python3 scripts/image-inputs.py >/dev/null
echo "==> portal: raw-colour guard + theme contrast tests"
sh scripts/check-portal-colors.sh
bun run --cwd web/apps/portal test
echo "==> browser player tests (the shared health cases among them)"
bun run --cwd web/packages/player test
echo "==> player themes (web/packages/ui-spec/themes up to date)"
bun scripts/export-player-themes.ts --check
echo "==> ui spec (icons, health issues and cases valid, every <Icon> exists)"
bun scripts/check-ui-spec.ts
echo "==> portal: typecheck + build"
bun run --cwd web/apps/portal build >/dev/null
echo "All checks passed."
