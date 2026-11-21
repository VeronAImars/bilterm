#!/usr/bin/env bash
# Regenerates every screenshot in docs/assets/ (README hero, docs/VISUAL_GALLERY.md, website).
#
#   scripts/gen-screenshots.sh [scene ...]      # default: all scenes
#
# Scenes are defined in examples/gen_screenshots.rs. They drive a headless WindowManager through the real
# Action/colon/key paths, render the real WorkspaceWidget into a ratatui Buffer and write SVG; this script
# rasterises the SVG to a palette PNG (deterministic: no PTY, clock or network). Needs inkscape (or
# rsvg-convert) and ImageMagick `convert`; optipng is used when present. The `dos` feature is built so the
# DOS-mode scene exists. Build output goes to $CARGO_TARGET_DIR (set it to a disk, never a tmpfs).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
work=$(mktemp -d "${TMPDIR:-$root/target}/shots.XXXXXX")
trap 'rm -rf "$work"' EXIT
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-4}
cargo build -q --example gen_screenshots --features dos
bin="${CARGO_TARGET_DIR:-$root/target}/debug/examples/gen_screenshots"
"$bin" "$work" "$@"
shopt -s nullglob
for svg in "$work"/screenshot-*.svg; do
  png="$root/docs/assets/$(basename "${svg%.svg}").png"
  if command -v inkscape >/dev/null; then
    inkscape "$svg" -o "$work/raw.png" >/dev/null 2>&1
  elif command -v rsvg-convert >/dev/null; then
    rsvg-convert "$svg" -o "$work/raw.png"
  else
    echo "need inkscape or rsvg-convert" >&2; exit 1
  fi
  convert "$work/raw.png" -dither None -colors 64 PNG8:"$png"
  command -v optipng >/dev/null && optipng -quiet -o2 "$png" || true
  echo "docs/assets/$(basename "$png")"
done
