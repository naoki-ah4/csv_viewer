#!/bin/bash
# assets/icon.svg から assets/icon.icns を作る（rsvg-convert と iconutil が要る）。
# できた .icns はリポジトリに入れてあるので、アイコンを変えたときだけ実行すればよい。
set -euo pipefail
cd "$(dirname "$0")/.."
SET="$(mktemp -d)/icon.iconset"
mkdir -p "$SET"
for s in 16 32 128 256 512; do
  rsvg-convert -w $s -h $s assets/icon.svg -o "$SET/icon_${s}x${s}.png"
  rsvg-convert -w $((s * 2)) -h $((s * 2)) assets/icon.svg -o "$SET/icon_${s}x${s}@2x.png"
done
iconutil -c icns "$SET" -o assets/icon.icns
echo "Built: assets/icon.icns"
