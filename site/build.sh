#!/usr/bin/env sh
# Assemble the LFD3K-SM website into one folder (default: _site at the repo root).
# Used by .github/workflows/pages.yml and for local previews:
#   sh site/build.sh [output-dir]
#   python -m http.server -d <output-dir>
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-"$root/_site"}

rm -rf "$out"
mkdir -p "$out/docs/video" "$out/docs/img" "$out/docs/phase2-bench/mockups" "$out/docs/phase4-bench/menu" "$out/multimode/preview"

# the pages
cp "$root"/site/index.html "$root"/site/docs.html "$root"/site/style.css "$root"/site/site.js "$root"/site/docs.js "$out/"

# images and animations the pages show
cp "$root"/docs/img/simian-logo.png "$out/docs/img/"
cp "$root"/docs/video/* "$out/docs/video/"
cp "$root"/docs/phase2-bench/*.png "$out/docs/phase2-bench/"
cp "$root"/docs/phase2-bench/mockups/*.gif "$root"/docs/phase2-bench/mockups/*.png "$out/docs/phase2-bench/mockups/"
rm -f "$out/docs/phase2-bench/mockups/idle_pw.png"   # never publish the password mockup
cp "$root"/docs/phase4-bench/menu/*.png "$out/docs/phase4-bench/menu/"
cp "$root"/multimode/preview/logo.gif "$root"/multimode/preview/menu.png "$out/multimode/preview/"

# the markdown docs.html renders
cp "$root"/README.md "$root"/NOTICE.md "$root"/THIRD_PARTY.md "$out/"
cp "$root"/docs/FEATURES.md "$root"/docs/2026-11-09-lfd3k-sm-design.md "$root"/docs/multimode.md \
   "$root"/docs/simnet.md "$root"/docs/phase2-bench.md "$root"/docs/phase3-bench.md \
   "$root"/docs/phase4-bench.md "$out/docs/"
cp "$root"/multimode/README.md "$out/multimode/"

echo "site assembled in $out"
