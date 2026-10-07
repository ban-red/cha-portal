#!/usr/bin/env sh
# Reclaim disk space from local dev artefacts. Safe by default: everything it
# removes is rebuilt on demand (the next build is just slower).
#
#   scripts/cleanup.sh              incremental caches, orphaned object files (macOS),
#                                   stale worktrees, Vite caches
#   scripts/cleanup.sh --deep       also `cargo clean` (all of target/)
#   scripts/cleanup.sh --docker     also prune stopped containers, dangling images
#                                   and the build cache (global, not just this repo)
#   scripts/cleanup.sh --dry-run    show sizes, remove nothing (combines with the above)
#
# Never touches data/, Docker volumes, or tagged images.
set -eu
cd "$(dirname "$0")/.."

deep=0 docker=0 dry=0
for arg in "$@"; do
  case "$arg" in
    --deep) deep=1 ;;
    --docker) docker=1 ;;
    --dry-run|-n) dry=1 ;;
    -h|--help) sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

size() { du -sh "$@" 2>/dev/null | awk '{print $1}'; }
free() { df -h . | awk 'NR==2 {print $4}'; }
run() { if [ "$dry" = 1 ]; then echo "   (dry run) $*"; else "$@"; fi; }
# rm -rf can fail with "Directory not empty" while a writer (rust-analyzer,
# cargo) recreates files mid-delete, so retry, and carry on if it still fails.
rmtree() {
  if [ "$dry" = 1 ]; then echo "   (dry run) rm -rf $1"; return 0; fi
  for _ in 1 2 3; do
    rm -rf "$1" 2>/dev/null && return 0
    sleep 1
  done
  echo "   could not fully remove $1 (is something still building?)" >&2
}

busy=0
if pgrep -x 'rust-analyzer|cargo|rustc' >/dev/null 2>&1 || pgrep -f 'rust-analyzer|/cargo |/rustc ' >/dev/null 2>&1; then
  busy=1
  echo "warning: rust-analyzer/cargo/rustc is running; it may recreate files while we delete" >&2
fi

echo "Free before: $(free)"

echo "==> cargo incremental caches"
for d in target/*/incremental; do
  [ -d "$d" ] || continue
  echo "   $d: $(size "$d")"
  rmtree "$d"
done

# On macOS a binary's debug info stays in the object files it was linked from,
# and each incremental rebuild writes new ones without deleting the old. Keep
# the objects some binary still points at (its OSO entries); drop the rest.
prune_objects() {
  deps=$1
  tmp=$(mktemp -d)
  find "$deps" "$(dirname "$deps")/examples" -maxdepth 1 -type f -perm -u+x \
    ! -name '*.o' ! -name '*.rlib' ! -name '*.rmeta' ! -name '*.d' 2>/dev/null |
    while IFS= read -r bin; do
      nm -ap "$bin" 2>/dev/null | sed -n 's/.* OSO //p'
    done | sed -n 's#.*/##; /\.o$/p' | sort -u >"$tmp/keep"
  find "$deps" -maxdepth 1 -name '*.o' | sed 's#.*/##' | sort >"$tmp/all"
  comm -23 "$tmp/all" "$tmp/keep" >"$tmp/drop"
  count=$(wc -l <"$tmp/drop" | tr -d ' ')
  bytes=$( (cd "$deps" && xargs stat -f '%z' <"$tmp/drop") | awk '{s+=$1} END {print s+0}')
  echo "   $deps: $count unreferenced objects, $(awk -v b="$bytes" 'BEGIN {printf "%.1fG", b/1e9}')"
  if [ "$dry" = 0 ] && [ "$count" -gt 0 ]; then
    (cd "$deps" && xargs rm -f <"$tmp/drop")
  fi
  rm -rf "$tmp"
}

if [ "$deep" = 0 ] && [ "$(uname)" = Darwin ] && command -v nm >/dev/null 2>&1; then
  echo "==> object files no binary references"
  if [ "$busy" = 1 ]; then
    echo "   skipped: a build is running and may not have linked its objects yet"
  else
    for d in target/*/deps; do
      [ -d "$d" ] && prune_objects "$d"
    done
  fi
fi

if [ "$deep" = 1 ]; then
  echo "==> cargo clean (target/: $(size target 2>/dev/null || echo 0))"
  run cargo clean
elif command -v cargo-sweep >/dev/null 2>&1; then
  echo "==> cargo sweep (artefacts unused for 3 days)"
  run cargo sweep --time 3
else
  echo "==> stale build variants: install cargo-sweep (cargo install cargo-sweep) to drop old ones,"
  echo "    or pass --deep to clear target/ entirely"
fi

echo "==> git worktrees"
run git worktree prune -v

echo "==> web caches"
for d in node_modules/.vite web/*/*/node_modules/.vite web/*/*/dist; do
  [ -d "$d" ] || continue
  echo "   $d: $(size "$d")"
  rmtree "$d"
done

if [ "$docker" = 1 ]; then
  if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    echo "==> docker (stopped containers, dangling images, build cache)"
    run docker container prune -f
    run docker image prune -f
    run docker builder prune -af
  else
    echo "==> docker not running, skipped"
  fi
fi

echo "Free after:  $(free)"
