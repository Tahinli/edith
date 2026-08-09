#!/usr/bin/env bash
# Regenerates the gitignored test fixtures under assets/.
# Needs the ffmpeg CLI (dev tool only — the app itself never uses ffmpeg).
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p assets
ffmpeg -y -f lavfi -i testsrc2=size=1280x720:rate=30:duration=5 \
    -c:v libx264 -profile:v baseline -pix_fmt yuv420p assets/test_baseline.mp4
ffmpeg -y -f lavfi -i testsrc2=size=1280x720:rate=30:duration=5 \
    -c:v libx264 -profile:v high -pix_fmt yuv420p assets/test_high.mp4
echo "fixtures written to assets/"
