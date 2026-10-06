#!/usr/bin/env sh
# Fails if the portal's components use colours that bypass the theme roles: Tailwind
# palette classes (bg-pink-500), white/black, or hex values. Colours belong in
# web/apps/portal/src/themes/*.css and style.css, which are not scanned.
set -eu
cd "$(dirname "$0")/.."

SRC=web/apps/portal/src
PALETTE='red|green|emerald|teal|amber|yellow|blue|sky|slate|zinc|gray|neutral|stone|rose|pink|fuchsia|purple|violet|indigo|cyan|lime|orange'
# Tailwind colour utilities (with variants and an optional /alpha) on palette shades, white or black.
PATTERN="(^|[^[:alnum:]_-])(text|bg|border|fill|stroke|ring|from|to|via|divide|outline|decoration|shadow|accent|caret)-(($PALETTE)-[0-9]+|white|black)([^[:alnum:]_-]|$)|#[0-9a-fA-F]{3}([0-9a-fA-F]{3})?([0-9a-fA-F]{2})?([^[:alnum:]_-]|$)"

# Allowed: the stream's own black background, which is theme-neutral by design.
hits=$(grep -rnE "$PATTERN" "$SRC" --include='*.vue' --include='*.ts' \
  --exclude-dir=themes --exclude=style.css \
  | grep -vE '^web/apps/portal/src/views/SessionView\.vue:[0-9]+:.*fixed inset-0 bg-black select-none' || true)

if [ -n "$hits" ]; then
  echo "Raw colours found. Use a theme role (text-ink, bg-panel, text-ok, ...) instead:" >&2
  echo "$hits" >&2
  exit 1
fi
echo "portal colours: no raw colours outside themes"
