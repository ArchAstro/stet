#!/bin/sh
# Regenerates the README screenshots in docs/screenshots from docs/showcase/essay.md.
# Off-screen renders (--screenshot), so they need no window and no remembered state.
# Usage: sh scripts/screenshots.sh [path/to/stet]   (default: target/release/stet)
set -eu
cd "$(dirname "$0")/.."
stet=${1:-target/release/stet}
[ -x "$stet" ] || cargo build --release
out=docs/screenshots
doc=docs/showcase/essay.md
mkdir -p "$out"

shot() { name=$1; shift; "$stet" --screenshot "$out/$name.png" --size 1280x800 "$@" "$doc"; }

shot hero        --keys ':margin<CR><C-w>w/oldest<CR>'
shot suggestions --keys '/refuse<CR>K'
shot menu        --keys '<D-/>sugg'
shot retouch     --keys '/proof.png<CR>:image<CR>'
shot tables      --keys '/Second<CR>zz'
shot code        --theme stet-ink --keys 'G'
shot files       --keys '<D-\>'
shot publish     --keys ':publish<CR>'

# One frame per theme for the theme strip.
for theme in stet stet-ink nord gruvbox-light dracula everforest; do
  "$stet" --screenshot "$out/theme-$theme.png" --size 900x560 --scale 1 --theme "$theme" --keys '/oldest<CR>' "$doc"
done
