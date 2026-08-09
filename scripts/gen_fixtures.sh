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
# A/V fixture: video + stereo AAC. Left channel 440 Hz, right 880 Hz, so
# channel mapping mistakes are audible; 1 Hz volume pulse makes drift visible.
ffmpeg -y -f lavfi -i testsrc2=size=1280x720:rate=30:duration=5 \
    -f lavfi -i "sine=frequency=440:duration=5" \
    -f lavfi -i "sine=frequency=880:duration=5" \
    -filter_complex "[1:a][2:a]join=inputs=2:channel_layout=stereo,volume='0.5+0.5*sin(2*PI*t)':eval=frame[a]" \
    -map 0:v -map "[a]" -c:v libx264 -profile:v baseline -pix_fmt yuv420p \
    -c:a aac -b:a 128k assets/test_av.mp4
# Second A/V fixture for multi-file import: same properties as test_av.mp4
# (1280x720@30 baseline, AAC-LC 44.1k stereo) but plainly different content —
# testsrc pattern, 4 s, an octave up (660/1320 Hz) with the same 1 Hz pulse.
ffmpeg -y -f lavfi -i testsrc=size=1280x720:rate=30:duration=4 \
    -f lavfi -i "sine=frequency=660:duration=4" \
    -f lavfi -i "sine=frequency=1320:duration=4" \
    -filter_complex "[1:a][2:a]join=inputs=2:channel_layout=stereo,volume='0.5+0.5*sin(2*PI*t)':eval=frame[a]" \
    -map 0:v -map "[a]" -c:v libx264 -profile:v baseline -pix_fmt yuv420p \
    -c:a aac -b:a 128k assets/test_av2.mp4
# Mismatch fixture: different resolution and no audio track at all, so import
# has something concrete to refuse.
ffmpeg -y -f lavfi -i testsrc2=size=640x360:rate=30:duration=2 \
    -an -c:v libx264 -profile:v baseline -pix_fmt yuv420p assets/test_mismatch.mp4
echo "fixtures written to assets/"
