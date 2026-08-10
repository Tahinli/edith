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
# Standalone audio fixtures: the same 440/880 Hz tone with the 1 Hz pulse the
# A/V fixture carries, at test_av.mp4's own 44.1k stereo -- so an imported song
# can share a timeline with the video clips and the peaks tests can reuse the
# dip pattern. One per container we claim to read.
tone="[0:a][1:a]join=inputs=2:channel_layout=stereo,volume='0.5+0.5*sin(2*PI*t)':eval=frame[a]"
for fmt in mp3 wav flac ogg m4a aac; do
    case $fmt in
        mp3) codec=(-c:a libmp3lame -b:a 128k) ;;
        wav) codec=(-c:a pcm_s16le) ;;
        flac) codec=(-c:a flac) ;;
        ogg) codec=(-c:a libvorbis) ;;
        # ALAC in mp4, and raw AAC in ADTS -- the two the mp4/AAC packet-copy
        # path cannot claim: one is not AAC, the other is not in an mp4.
        m4a) codec=(-c:a alac) ;;
        aac) codec=(-c:a aac -b:a 128k -f adts) ;;
    esac
    ffmpeg -y -f lavfi -i "sine=frequency=440:duration=3" \
        -f lavfi -i "sine=frequency=880:duration=3" \
        -filter_complex "$tone" -map "[a]" -ar 44100 "${codec[@]}" \
        "assets/test_tone.$fmt"
done
# The refusal fixture: same tone at 48k, which one output device cannot mix
# with a 44.1k timeline.
ffmpeg -y -f lavfi -i "sine=frequency=440:duration=3" \
    -f lavfi -i "sine=frequency=880:duration=3" \
    -filter_complex "$tone" -map "[a]" -ar 48000 -c:a pcm_s16le \
    assets/test_tone_48k.wav
echo "fixtures written to assets/"
