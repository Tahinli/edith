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
# Multi-audio fixture: one video and three audio streams, so stream selection
# has something to select. Stream 0 is AAC 44.1k stereo (440 Hz left, 880 Hz
# right), stream 1 is AAC 22.05k mono 220 Hz tagged French — different rate,
# layout and content, so a test can tell which stream it was handed — and
# stream 2 is AC-3, the codec we cannot decode and must still list.
ffmpeg -y -f lavfi -i testsrc2=size=320x240:rate=30:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2" \
    -f lavfi -i "sine=frequency=880:duration=2" \
    -f lavfi -i "sine=frequency=220:duration=2" \
    -f lavfi -i "sine=frequency=330:duration=2" \
    -filter_complex "[1:a][2:a]join=inputs=2:channel_layout=stereo[a0]" \
    -map 0:v -map "[a0]" -map 3:a -map 4:a \
    -c:v libx264 -profile:v baseline -pix_fmt yuv420p \
    -c:a:0 aac -ar:a:0 44100 -ac:a:0 2 \
    -c:a:1 aac -ar:a:1 22050 -ac:a:1 1 \
    -c:a:2 ac3 -ar:a:2 44100 -ac:a:2 2 \
    -metadata:s:a:1 language=fra assets/test_multiaudio.mp4
# Unsupported-audio fixture: an AC-3 track, which we cannot decode at all. The
# file plays as picture with silence, and the point is that it says so.
ffmpeg -y -f lavfi -i testsrc2=size=320x180:rate=30:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2" \
    -map 0:v -map 1:a -c:v libx264 -profile:v baseline -pix_fmt yuv420p \
    -c:a ac3 -b:a 192k assets/test_ac3.mp4
# Multi-language fixture: the shape real media has — one video and two AAC
# streams that *agree* on rate and layout (44.1k stereo) and differ only in
# language and content, 440/880 Hz undefined and 220/330 Hz French. One
# timeline means one set of audio parameters, so this is the file where the
# second stream can actually be put on the timeline; test_multiaudio.mp4 is the
# file where it cannot, and both cases have to be shown.
ffmpeg -y -f lavfi -i testsrc2=size=320x240:rate=30:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2" \
    -f lavfi -i "sine=frequency=880:duration=2" \
    -f lavfi -i "sine=frequency=220:duration=2" \
    -f lavfi -i "sine=frequency=330:duration=2" \
    -filter_complex "[1:a][2:a]join=inputs=2:channel_layout=stereo[a0];\
[3:a][4:a]join=inputs=2:channel_layout=stereo[a1]" \
    -map 0:v -map "[a0]" -map "[a1]" \
    -c:v libx264 -profile:v baseline -pix_fmt yuv420p \
    -c:a aac -ar 44100 -ac 2 \
    -metadata:s:a:1 language=fra assets/test_multilang.mp4
# VP9 fixture: the same A/V shape as test_av.mp4 (1280x720@30, AAC-LC 44.1k
# stereo) with a VP9 picture, so what the tests measure is the codec and
# nothing else. 2 s, because VP9 only ever decodes on the hardware path.
ffmpeg -y -f lavfi -i testsrc2=size=1280x720:rate=30:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2" \
    -f lavfi -i "sine=frequency=880:duration=2" \
    -filter_complex "[1:a][2:a]join=inputs=2:channel_layout=stereo,volume='0.5+0.5*sin(2*PI*t)':eval=frame[a]" \
    -map 0:v -map "[a]" -c:v libvpx-vp9 -b:v 2M -pix_fmt yuv420p \
    -c:a aac -b:a 128k assets/test_vp9.mp4
# HEVC fixture: the same A/V shape again, so the tests measure the codec and
# nothing else. `-tag:v hev1` is not a preference: mp4 0.14 only recognises a
# hev1 sample entry, so an hvc1-tagged file would read back as no video track at
# all. 8-bit Main profile, because the plugin's NV12 read-back cannot carry
# Main 10.
ffmpeg -y -f lavfi -i testsrc2=size=1280x720:rate=30:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2" \
    -f lavfi -i "sine=frequency=880:duration=2" \
    -filter_complex "[1:a][2:a]join=inputs=2:channel_layout=stereo,volume='0.5+0.5*sin(2*PI*t)':eval=frame[a]" \
    -map 0:v -map "[a]" -c:v libx265 -tag:v hev1 -pix_fmt yuv420p \
    -c:a aac -b:a 128k assets/test_hevc.mp4
# AV1 fixture, and the only Matroska one: `mp4 0.14` has no `av01` sample entry
# at all, so AV1 is read out of an mkv (`demux::MkvDemuxer`). Same A/V shape as
# the others so the tests measure the codec and nothing else -- but `-g 30` is
# not a preference: it puts a second keyframe at frame 30, which is what the
# sync index and the seek test are checked against. 8-bit Main profile, because
# the plugin's NV12 read-back cannot carry 10-bit. Its AAC track is deliberate
# too: Matroska audio is not wired to the decoder yet, and the notice that says
# so needs a track to name.
ffmpeg -y -f lavfi -i testsrc2=size=1280x720:rate=30:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2" \
    -f lavfi -i "sine=frequency=880:duration=2" \
    -filter_complex "[1:a][2:a]join=inputs=2:channel_layout=stereo,volume='0.5+0.5*sin(2*PI*t)':eval=frame[a]" \
    -map 0:v -map "[a]" -c:v libsvtav1 -preset 8 -g 30 -pix_fmt yuv420p \
    -c:a aac -b:a 128k assets/test_av1.mkv
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
