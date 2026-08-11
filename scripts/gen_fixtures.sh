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
# The shape a BluRay remux has: 48 kHz 5.1 AC-3, which the decoder downmixes to
# stereo for the timeline. The mono fixture above is the other end of the same
# path — no downmix at all — and both have to work.
ffmpeg -y -f lavfi -i testsrc2=size=320x180:rate=30:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2:sample_rate=48000" \
    -map 0:v -map 1:a -c:v libx264 -profile:v baseline -pix_fmt yuv420p \
    -ac 6 -c:a ac3 -b:a 448k assets/test_ac3_51.mp4
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
# nothing else. 8-bit Main profile, because the plugin's NV12 read-back cannot
# carry Main 10.
ffmpeg -y -f lavfi -i testsrc2=size=1280x720:rate=30:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2" \
    -f lavfi -i "sine=frequency=880:duration=2" \
    -filter_complex "[1:a][2:a]join=inputs=2:channel_layout=stereo,volume='0.5+0.5*sin(2*PI*t)':eval=frame[a]" \
    -map 0:v -map "[a]" -c:v libx265 -tag:v hev1 -pix_fmt yuv420p \
    -c:a aac -b:a 128k assets/test_hevc.mp4
# The same HEVC stream tagged hvc1, which is what Apple and ffmpeg's mov muxer
# write in practice: mp4 0.14 recognises only a hev1 sample entry, so this one
# is found by demux.rs reading the stsd fourcc itself (`tests/hevc_hvc1.rs`).
ffmpeg -y -f lavfi -i testsrc2=size=1280x720:rate=30:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2" \
    -f lavfi -i "sine=frequency=880:duration=2" \
    -filter_complex "[1:a][2:a]join=inputs=2:channel_layout=stereo,volume='0.5+0.5*sin(2*PI*t)':eval=frame[a]" \
    -map 0:v -map "[a]" -c:v libx265 -tag:v hvc1 -pix_fmt yuv420p \
    -c:a aac -b:a 128k assets/test_hevc_hvc1.mp4
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
# H.264 in Matroska: the same stream the mp4 fixtures carry, in the container
# that used to refuse it. Baseline profile so `rusty_h264` reads it with nothing
# installed, `-g 30` for the second keyframe the sync index is checked against,
# and the AAC track for the sound an mkv's audio path reads back.
ffmpeg -y -f lavfi -i testsrc2=size=1280x720:rate=30:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2" \
    -f lavfi -i "sine=frequency=880:duration=2" \
    -filter_complex "[1:a][2:a]join=inputs=2:channel_layout=stereo,volume='0.5+0.5*sin(2*PI*t)':eval=frame[a]" \
    -map 0:v -map "[a]" -c:v libx264 -profile:v baseline -g 30 -pix_fmt yuv420p \
    -c:a aac -b:a 128k assets/test_h264.mkv
# HEVC in Matroska: the shape a film off a disc arrives in. Same A/V shape as
# test_hevc.mp4 so the tests measure the container and nothing else, `-g 30` for
# the same reason test_av1.mkv has it (a second keyframe to seek to), and the
# AAC track is read out of the mkv now rather than named as unsupported.
ffmpeg -y -f lavfi -i testsrc2=size=1280x720:rate=30:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2" \
    -f lavfi -i "sine=frequency=880:duration=2" \
    -filter_complex "[1:a][2:a]join=inputs=2:channel_layout=stereo,volume='0.5+0.5*sin(2*PI*t)':eval=frame[a]" \
    -map 0:v -map "[a]" -c:v libx265 -x265-params log-level=error -g 30 -pix_fmt yuv420p \
    -c:a aac -b:a 128k assets/test_hevc.mkv
# ...and the same file in the shape the ask came from: HEVC **Main 10** with a
# 5.1 AAC track, which is a 4K BluRay remux in miniature. The picture decodes
# through a P010 surface pool and the sound is folded to stereo, so both halves
# of that file have a fixture. One tone per channel -- 440 FL, 880 FR, 220 FC,
# 60 LFE, 1320 BL, 1760 BR as written, whatever the encoder's own element order
# does with them -- so a test can say which channel reached which output.
ffmpeg -y -f lavfi -i testsrc2=size=1280x720:rate=30:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2:sample_rate=48000" \
    -f lavfi -i "sine=frequency=880:duration=2:sample_rate=48000" \
    -f lavfi -i "sine=frequency=220:duration=2:sample_rate=48000" \
    -f lavfi -i "sine=frequency=60:duration=2:sample_rate=48000" \
    -f lavfi -i "sine=frequency=1320:duration=2:sample_rate=48000" \
    -f lavfi -i "sine=frequency=1760:duration=2:sample_rate=48000" \
    -filter_complex "[1:a][2:a][3:a][4:a][5:a][6:a]join=inputs=6:channel_layout=5.1[a]" \
    -map 0:v -map "[a]" -c:v libx265 -x265-params log-level=error -g 30 -pix_fmt yuv420p10le \
    -c:a aac -b:a 384k assets/test_hevc10.mkv
# Sync fixture: one flash and one beep, at the same instant. Black picture with
# a white frame from t=1.0 to t=1.1, silence with a 1 kHz tone over exactly that
# stretch — so a test can find each of them and say how far apart they came out.
# What it is for is *speed*: a re-timed clip that drifts puts the beep somewhere
# the flash is not, and nothing else in the fixture set has a mark to measure
# that against. Same 30 fps / 44.1k stereo shape as test_av.mp4, so it can share
# a timeline with the rest.
# The beep is silence-then-tone-then-silence rather than a gated tone: lavfi's
# `sine` comes out at an eighth of full scale, and a mark a test looks for has
# to be plainly louder than anything around it.
ffmpeg -y -f lavfi -i "color=c=black:s=320x180:r=30:d=3" \
    -f lavfi -i "sine=frequency=1000:duration=0.1:sample_rate=44100" \
    -filter_complex "[0:v]drawbox=x=0:y=0:w=iw:h=ih:color=white:t=fill:\
enable='between(t,1,1.1)'[v];\
[1:a]volume=6,aformat=channel_layouts=stereo,adelay=1000|1000,apad=whole_dur=3[a]" \
    -map "[v]" -map "[a]" -c:v libx264 -profile:v baseline -pix_fmt yuv420p \
    -c:a aac -b:a 128k assets/test_speed_sync.mp4
# Mismatch fixture: different resolution and no audio track at all, so import
# has something concrete to refuse.
ffmpeg -y -f lavfi -i testsrc2=size=640x360:rate=30:duration=2 \
    -an -c:v libx264 -profile:v baseline -pix_fmt yuv420p assets/test_mismatch.mp4
# Frame-rate fixture: test_av.mp4 in every respect a timeline is held to -- same
# codec, same profile, same 44.1k stereo AAC -- except that it runs at 25 fps,
# so a timeline of 30 fps clips has one at another rate to read through `Rate`.
ffmpeg -y -f lavfi -i testsrc2=size=1280x720:rate=25:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2" \
    -f lavfi -i "sine=frequency=880:duration=2" \
    -filter_complex "[1:a][2:a]join=inputs=2:channel_layout=stereo,volume='0.5+0.5*sin(2*PI*t)':eval=frame[a]" \
    -map 0:v -map "[a]" -c:v libx264 -profile:v baseline -pix_fmt yuv420p \
    -c:a aac -b:a 128k assets/test_25fps.mp4
# ...and the same again at 23.976 fps (24000/1001), the rate whose ratio to a
# 30 fps timeline does not terminate: what the frame mapping's rounding is
# actually tested against.
ffmpeg -y -f lavfi -i testsrc2=size=1280x720:rate=24000/1001:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2" \
    -f lavfi -i "sine=frequency=880:duration=2" \
    -filter_complex "[1:a][2:a]join=inputs=2:channel_layout=stereo,volume='0.5+0.5*sin(2*PI*t)':eval=frame[a]" \
    -map 0:v -map "[a]" -c:v libx264 -profile:v baseline -pix_fmt yuv420p \
    -c:a aac -b:a 128k assets/test_23976fps.mp4
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
# Still-image fixture: the source with a picture and no timeline in it. Two
# bands rather than one colour, so a test can tell top from bottom (a flipped
# decode) and red from blue (a swapped channel order); 640x360, which is 16:9
# like the video fixtures, so a Fit onto 1280x720 covers the canvas with no
# bars and every pixel of the exported frame is the image.
ffmpeg -y -f lavfi -i "color=c=red:s=640x180,format=rgb24" \
    -f lavfi -i "color=c=blue:s=640x180,format=rgb24" \
    -filter_complex "[0:v][1:v]vstack=inputs=2,format=rgb24" \
    -frames:v 1 assets/test_still.png
# The refusal fixture: same tone at 48k, which one output device cannot mix
# with a 44.1k timeline.
ffmpeg -y -f lavfi -i "sine=frequency=440:duration=3" \
    -f lavfi -i "sine=frequency=880:duration=3" \
    -filter_complex "$tone" -map "[a]" -ar 48000 -c:a pcm_s16le \
    assets/test_tone_48k.wav
echo "fixtures written to assets/"
