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
# The same VP9 stream in Matroska, which is where a yt-dlp download and every
# other VP9 file in the wild actually arrives: `V_VP9` used to fall through the
# mkv track dispatch and be refused by name while the mp4 twin above decoded.
# Opus audio because that is what a .webm carries -- the sound decodes here now
# (`ruopus`), and this is the webm cell of the Opus row in `capability_matrix`.
ffmpeg -y -f lavfi -i testsrc2=size=1280x720:rate=30:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2" \
    -c:v libvpx-vp9 -b:v 2M -g 30 -pix_fmt yuv420p \
    -c:a libopus -b:a 96k assets/test_vp9.webm
# ...and profile 2, 10-bit, which no container states: an mkv `TrackEntry`
# carries no configuration record for VP9 at all and the `vpcC` in an mp4 is
# optional, so the depth is read off the keyframe's uncompressed header
# (`demux::vp9_bit_depth`) -- the difference between a P010 surface pool and a
# picture of garbage.
ffmpeg -y -f lavfi -i testsrc2=size=1280x720:rate=30:duration=2 \
    -c:v libvpx-vp9 -b:v 2M -g 30 -profile:v 2 -pix_fmt yuv420p10le \
    -an assets/test_vp9_10.webm
# One Matroska file per audio codec the refusal string in `audio.rs` claims is
# decodable, as five tracks of one file: FLAC, MP3, Vorbis, ALAC and PCM. The
# string said "AAC and AC-3 only" long after every one of these decoded, which is
# the shape of refusal this suite exists to stop (`tests/capability_matrix.rs`).
ffmpeg -y -f lavfi -i testsrc2=size=320x240:rate=30:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2" \
    -map 0:v -map 1:a -map 1:a -map 1:a -map 1:a -map 1:a \
    -c:v libx264 -profile:v baseline -pix_fmt yuv420p \
    -c:a:0 flac -c:a:1 libmp3lame -c:a:2 libvorbis -c:a:3 alac -c:a:4 pcm_s16le \
    assets/test_mkv_audio.mkv
# ...and the codec that really is refused, so the other half of the refusal is
# testable too: DTS, which no decoder in this tree has (symphonia has none at any
# version and there is no pure-Rust one to reach for, unlike AC-3 and Opus). The
# notice must name *this* track and go on naming what would have worked.
ffmpeg -y -f lavfi -i testsrc2=size=320x240:rate=30:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2:sample_rate=48000" \
    -map 0:v -map 1:a -c:v libx264 -profile:v baseline -pix_fmt yuv420p \
    -c:a dca -strict -2 -ac 2 assets/test_dts.mkv
# Two video tracks in one mp4, at different sizes so a test can tell which one
# the demuxer picked. `Mp4Reader::tracks()` is a HashMap and iterating it made
# "the video track" a different one from one run to the next; the pick comes out
# of `moov.traks` (file order) and this is what holds it there.
ffmpeg -y -f lavfi -i testsrc2=size=1280x720:rate=30:duration=2 \
    -f lavfi -i testsrc=size=320x240:rate=30:duration=2 \
    -map 0:v -map 1:v -c:v libx264 -profile:v baseline -pix_fmt yuv420p \
    assets/test_two_video.mp4
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
# sync index and the seek test are checked against. 8-bit Main profile here; the
# 10-bit twin is below. Its AAC track is deliberate too: it is the AAC a Matroska
# file's sound is read out of through symphonia, beside the AC-3 fixtures further
# down that no symphonia version decodes.
ffmpeg -y -f lavfi -i testsrc2=size=1280x720:rate=30:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2" \
    -f lavfi -i "sine=frequency=880:duration=2" \
    -filter_complex "[1:a][2:a]join=inputs=2:channel_layout=stereo,volume='0.5+0.5*sin(2*PI*t)':eval=frame[a]" \
    -map 0:v -map "[a]" -c:v libsvtav1 -preset 8 -g 30 -pix_fmt yuv420p \
    -c:a aac -b:a 128k assets/test_av1.mkv
# ...and an AV1 file that asks for film grain, which no other fixture here does.
# A frame with `apply_grain` set is *displayed* from a second surface the driver
# synthesizes the grain into, and a decoder that hands it none is refused the
# picture outright (radeonsi: VA_STATUS_ERROR_INVALID_SURFACE out of
# `vaEndPicture`) -- which is how AV1 decode died at frame 49 of a real film
# while every fixture above sailed through. Three seconds rather than two so the
# surface pool wraps several times, `noise` on the source because SVT-AV1 reads
# the grain parameters it signals *off the source* (a clean testsrc2 yields grain
# with no amplitude at all, measured), and `film-grain-denoise=1` so the coded
# picture is the smooth one and the grain is really the decoder's own work.
ffmpeg -y -f lavfi -i "testsrc2=size=1280x720:rate=30:duration=3,noise=alls=24:allf=t+u" \
    -f lavfi -i "sine=frequency=440:duration=3" \
    -f lavfi -i "sine=frequency=880:duration=3" \
    -filter_complex "[1:a][2:a]join=inputs=2:channel_layout=stereo,volume='0.5+0.5*sin(2*PI*t)':eval=frame[a]" \
    -map 0:v -map "[a]" -c:v libsvtav1 -preset 8 -g 30 -pix_fmt yuv420p \
    -svtav1-params film-grain=40:film-grain-denoise=1 \
    -c:a aac -b:a 128k assets/test_av1_grain.mkv
# ...and the same file 10-bit, which is what an `av1C` with `high_bitdepth` set
# reads as and what the P010 surface pool is picked by -- the AV1 seat of the
# pair test_hevc.mkv/test_hevc10.mkv already make for HEVC. libaom rather than
# SVT-AV1 because it is the encoder that writes a 10-bit `av1C` here.
ffmpeg -y -f lavfi -i testsrc2=size=1280x720:rate=30:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2" \
    -f lavfi -i "sine=frequency=880:duration=2" \
    -filter_complex "[1:a][2:a]join=inputs=2:channel_layout=stereo,volume='0.5+0.5*sin(2*PI*t)':eval=frame[a]" \
    -map 0:v -map "[a]" -c:v libaom-av1 -cpu-used 8 -row-mt 1 -g 30 -pix_fmt yuv420p10le \
    -c:a aac -b:a 128k assets/test_av1_10.mkv
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
# Subtitles in Matroska: the two text codecs a film carries, muxed out of the
# hand-written files the tests also parse standalone
# (`crates/engine/tests/data/`), so the embedded cues and the external ones can
# be compared to each other and both to the source. Track 2 is S_TEXT/UTF8 and
# track 3 is S_TEXT/ASS -- the same three cues, one of them with markup, which
# is what makes "the markup is resolved, not carried" measurable.
#
# No bitmap track here: ffmpeg cannot encode text subtitles into pictures, so
# the PGS refusal is checked against a Matroska file the test writes itself
# (`crates/engine/tests/subtitles.rs`).
ffmpeg -y -f lavfi -i testsrc2=size=320x240:rate=30:duration=5 \
    -i crates/engine/tests/data/test_subs.srt \
    -i crates/engine/tests/data/test_subs.ass \
    -map 0:v -map 1:0 -map 2:0 \
    -c:v libsvtav1 -preset 8 -g 30 -pix_fmt yuv420p \
    -c:s:0 srt -metadata:s:s:0 language=eng \
    -c:s:1 ass -metadata:s:s:1 language=fra -metadata:s:s:1 title=Signs \
    assets/test_subs.mkv
# The same two tracks with no picture and no sound at all: `.mks`, the Matroska
# extension that is the subtitles alone (what a subtitle release ships beside
# the film). Refused by name until the container gate learned every Matroska
# extension rather than the two that carry a film -- the very reader that walks
# a `.mkv` walks this.
ffmpeg -y -i crates/engine/tests/data/test_subs.srt \
    -i crates/engine/tests/data/test_subs.ass -map 0:0 -map 1:0 \
    -c:s:0 srt -metadata:s:s:0 language=eng \
    -c:s:1 ass -metadata:s:s:1 language=fra -metadata:s:s:1 title=Signs \
    -f matroska assets/test_subs.mks
# ...and the same file stating its languages the way a modern muxer does: in
# `LanguageBCP47` (0x22B59D) and *not* in the legacy `Language` (0x22B59C),
# which is the shape of every English track of two real Blu-ray-sourced films.
# ffmpeg writes only the legacy element (8.1.2 does not read
# the BCP-47 one either), so the element is swapped in place afterwards: the
# 4-byte id, then the length as a two-byte VINT (`40 02`, a legal non-minimal
# encoding) so the tag is two bytes and the element stays the eight bytes it
# was -- every enclosing size in the file survives untouched.
python3 - <<'PATCH'
data = open("assets/test_subs.mkv", "rb").read()
size = len(data)
for legacy, tag in ((b"eng", b"en"), (b"fra", b"fr")):
    data = data.replace(b"\x22\xb5\x9c\x83" + legacy, b"\x22\xb5\x9d\x40\x02" + tag)
assert len(data) == size, "the swap has to be size-for-size"
assert b"\x22\xb5\x9d" in data, "and it has to have happened"
open("assets/test_subs_bcp47.mkv", "wb").write(data)
PATCH
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
# Matroska sound: the two Dolby codecs that arrive in one and are read straight
# out of its blocks (`demux::MkvAudio`). Stereo AC-3 for the plain syntax and
# **5.1 E-AC-3** for Annex E -- the shape a remux has, and the one that must
# come down to stereo through the same A/52 7.8 downmix the mp4 path uses.
# 48 kHz both, which is the only rate E-AC-3 is written at in practice. Small
# H.264 picture on purpose: these two are about the sound, and the software
# decoder reads that one, so an audio test needs no VA-API plugin to open a
# session on them.
ffmpeg -y -f lavfi -i testsrc2=size=320x180:rate=30:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2:sample_rate=48000" \
    -f lavfi -i "sine=frequency=880:duration=2:sample_rate=48000" \
    -filter_complex "[1:a][2:a]join=inputs=2:channel_layout=stereo[a]" \
    -map 0:v -map "[a]" -c:v libx264 -profile:v baseline -g 30 -pix_fmt yuv420p \
    -c:a ac3 -b:a 192k assets/test_ac3.mkv
ffmpeg -y -f lavfi -i testsrc2=size=320x180:rate=30:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2:sample_rate=48000" \
    -map 0:v -map 1:a -c:v libx264 -profile:v baseline -g 30 -pix_fmt yuv420p \
    -ac 6 -c:a eac3 -b:a 384k assets/test_eac3.mkv
# Dual audio in Matroska: the shape an anime remux has -- one picture and two
# AAC tracks that differ in language and content and *agree* on rate and layout
# (44.1k stereo), so either one can go on the same timeline. Stream 0 is
# 440/880 Hz English, stream 1 is 220/330 Hz French; the tones are what a test
# tells the two apart by, and getting the mapping wrong plays the other
# language. test_multilang.mp4 is this file's mp4 twin.
ffmpeg -y -f lavfi -i testsrc2=size=320x180:rate=30:duration=2 \
    -f lavfi -i "sine=frequency=440:duration=2" \
    -f lavfi -i "sine=frequency=880:duration=2" \
    -f lavfi -i "sine=frequency=220:duration=2" \
    -f lavfi -i "sine=frequency=330:duration=2" \
    -filter_complex "[1:a][2:a]join=inputs=2:channel_layout=stereo[a0];\
[3:a][4:a]join=inputs=2:channel_layout=stereo[a1]" \
    -map 0:v -map "[a0]" -map "[a1]" \
    -c:v libx264 -profile:v baseline -g 30 -pix_fmt yuv420p \
    -c:a aac -ar 44100 -ac 2 \
    -metadata:s:a:0 language=eng -metadata:s:a:1 language=fra \
    assets/test_multiaudio.mkv
# Colour-tag fixtures. The picture is irrelevant here -- what is measured is
# what the file *says* its numbers mean (`engine::colorspace`), so these are
# one-second clips with nothing in them.
#
# First pair: SMPTE 170M (BT.601) tagged in the container, at 720 lines, where
# the resolution heuristic would have said BT.709. That is the point: it is the
# fixture that shows the container tags being read and outranking the guess.
# Both containers, because the `colr` box and the Matroska `Colour` element are
# parsed by different code.
for out in assets/test_bt601.mp4 assets/test_bt601.mkv; do
    ffmpeg -y -f lavfi -i testsrc2=size=1280x720:rate=30:duration=1 \
        -c:v libx264 -profile:v baseline -pix_fmt yuv420p \
        -colorspace smpte170m -color_primaries smpte170m -color_trc smpte170m \
        "$out"
done
# ...and the tier below it: BT.709 written into the *bitstream* by the encoder
# and nowhere else. Passing the codes through `-x264-params`/`-x265-params`/
# `-svtav1-params` rather than through ffmpeg's stream tags leaves the muxer
# with nothing to write, so these files have no `colr` box at all -- checked by
# the test. 480 lines, so the heuristic would say BT.601 and a pass means the
# VUI (or AV1 `color_config`) was really read. The HEVC one asks for
# `scaling-lists=default` on purpose: a set of scaling lists sits between the
# SPS header and its VUI, and a walk that counts them wrongly reads the colour
# out of the middle of a coefficient. (x264 writes its quant matrices into the
# PPS instead, so the H.264 side of that is a synthetic SPS in the unit tests.)
ffmpeg -y -f lavfi -i testsrc2=size=640x480:rate=30:duration=1 \
    -c:v libx264 -profile:v high -pix_fmt yuv420p \
    -x264-params colormatrix=bt709:transfer=bt709:colorprim=bt709 \
    assets/test_vui_h264.mp4
ffmpeg -y -f lavfi -i testsrc2=size=640x480:rate=30:duration=1 \
    -c:v libx265 -pix_fmt yuv420p \
    -x265-params log-level=error:scaling-lists=default:colormatrix=bt709:transfer=bt709:colorprim=bt709 \
    assets/test_vui_hevc.mp4
ffmpeg -y -f lavfi -i testsrc2=size=640x480:rate=30:duration=1 \
    -c:v libsvtav1 -preset 8 -pix_fmt yuv420p \
    -svtav1-params color-primaries=1:transfer-characteristics=1:matrix-coefficients=1 \
    assets/test_vui_av1.mp4
# The HDR fixture, and the one place in this set where the *picture* is the
# point: four flat patches written straight into the planes by `geq`, so a test
# can name the code it feeds the tone map (`engine::tonemap`) and the code it
# expects back. Top left is BT.2408 diffuse white -- 203 cd/m^2 is PQ signal
# 0.584, limited-range code 144 -- top right the 1000 cd/m^2 the grade is
# mastered against (code 181), and the bottom half a saturated BT.2020 red,
# which is what a grey-washed render loses. H.264 rather than HEVC so the
# software decoder can read it on a machine with no VA-API plugin: the tone map
# is fed by the tags, not by the codec. `-qp 1` on flat patches is exact to the
# code, so the numbers a test names are the numbers that arrive; the tags go on
# through `setparams` rather than through the encoder's own `-colorspace`,
# because that one makes ffmpeg *convert* into BT.2020 and the patches would no
# longer be the codes written above.
ffmpeg -y -f lavfi -i "color=c=black:s=320x240:rate=25:duration=1,format=yuv420p,geq=lum='if(lt(Y,H/2),if(lt(X,W/2),144,181),100)':cb='if(lt(Y,H/2),128,90)':cr='if(lt(Y,H/2),128,200)',setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc:range=tv" \
    -c:v libx264 -profile:v high -qp 1 -pix_fmt yuv420p \
    assets/test_pq.mp4
# The HDR *metadata* fixtures: the same grade written three ways, so the three
# places a film's real peak brightness can live each have a file with known
# numbers in it. MaxCLL 1000, MaxFALL 400, mastering display 1000 nits down to
# 0.005 (which is what x265's `L(10000000,50)` means: ten-thousandths of a nit).
#
# Two passes, and the second one is not optional: `-x265-params master-display`
# only writes the SEI messages *inside the bitstream*, and ffmpeg's muxers write
# a container's own MaxCLL/`mdcv` off stream side data, which exists only once a
# decode has extracted those SEIs. Remuxing with `-c copy` does not do it --
# verified, the `Colour` element comes back out with the code points and nothing
# else. So: encode HEVC with the SEIs, then transcode into each container, which
# is also what leaves test_hdr_sei.mkv carrying the SEI tier *alone* (H.264
# writes no such SEI, so the transcoded pair carry the container tier alone).
ffmpeg -y -f lavfi -i "color=c=black:s=320x240:rate=25:duration=0.4,format=yuv420p10le,setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc:range=tv" \
    -c:v libx265 -pix_fmt yuv420p10le \
    -x265-params "log-level=error:master-display=G(8500,39850)B(6550,2300)R(35400,14600)WP(15635,16450)L(10000000,50):max-cll=1000,400" \
    assets/test_hdr_sei.mkv
ffmpeg -y -i assets/test_hdr_sei.mkv -c:v libx264 -qp 20 -pix_fmt yuv420p \
    assets/test_hdr_meta.mkv
ffmpeg -y -i assets/test_hdr_sei.mkv -c:v libx264 -qp 20 -pix_fmt yuv420p \
    assets/test_hdr_meta.mp4
# ...and one whose declared peak is a number nothing assumes: the three above
# say 1000, which is exactly what a file that declared *nothing* is assumed at,
# so no picture made from them can show that the metadata was read. This one is
# mastered at 4000 (`L(40000000,50)`, MaxCLL 4000) over the same four patches
# `test_pq.mp4` carries -- ten-bit codes here, four times the eight-bit ones --
# so a tone map told 4000 lands them visibly elsewhere than one told 1000.
#
# Same two passes for the same reason, and the H.264 second one is what makes it
# software-decodable: the render tests that read it must not need a VA-API
# plugin. `-qp 1` on flat patches so the codes arrive as written (measured: 144,
# 181 and 100 come back exactly).
bright_sei="$(mktemp -t edith_hdr_bright_XXXXXX.mkv)"
ffmpeg -y -f lavfi -i "color=c=black:s=320x240:rate=25:duration=0.4,format=yuv420p10le,geq=lum='if(lt(Y,H/2),if(lt(X,W/2),576,724),400)':cb='if(lt(Y,H/2),512,360)':cr='if(lt(Y,H/2),512,800)',setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc:range=tv" \
    -c:v libx265 -pix_fmt yuv420p10le \
    -x265-params "log-level=error:master-display=G(8500,39850)B(6550,2300)R(35400,14600)WP(15635,16450)L(40000000,50):max-cll=4000,400" \
    "$bright_sei"
ffmpeg -y -i "$bright_sei" -c:v libx264 -qp 1 -pix_fmt yuv420p \
    assets/test_hdr_bright.mkv
rm -f "$bright_sei"
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
# The two Opus fixtures, which cannot join the loop above: Opus decodes at 48 kHz
# and nothing else, so these are the 48k twins of the tone rather than 44.1k ones.
# First the standalone `.opus`, an Ogg file `crate::is_audio` now admits.
ffmpeg -y -f lavfi -i "sine=frequency=440:duration=3:sample_rate=48000" \
    -f lavfi -i "sine=frequency=880:duration=3:sample_rate=48000" \
    -filter_complex "$tone" -map "[a]" -c:a libopus -b:a 96k \
    assets/test_tone.opus
# ...and 5.1 Opus in an `.mka`, which is the shape a film soundtrack has: four
# Opus streams with the Vorbis channel mapping, folded to stereo on the way to
# the timeline. The tone sits in **FL and BR only**, both silent in between, so
# the fold's channel order is checkable without an FFT: done right, 440 Hz comes
# out left and 880 Hz right; read in the decoder's own Vorbis order (FL, FC, FR,
# BL, BR, LFE) instead of the film order the fold wants (FL, FR, FC, LFE, BL, BR),
# BR lands on the left with FL and the right channel comes out silent.
# `join` needs every output channel mapped and assigns them by *name*, not by
# input order: without the explicit `map=` the two tones land in FC and BR
# instead, and the fixture stops testing what its comment says.
ffmpeg -y -f lavfi -i "sine=frequency=440:duration=3:sample_rate=48000" \
    -f lavfi -i "sine=frequency=880:duration=3:sample_rate=48000" \
    -f lavfi -i "anullsrc=r=48000:cl=quad:d=3" \
    -filter_complex "[0:a][1:a][2:a]join=inputs=3:channel_layout=5.1:map=0.0-FL|2.0-FR|2.1-FC|2.2-LFE|2.3-BL|1.0-BR[a]" \
    -map "[a]" -c:a libopus -b:a 256k assets/test_opus_51.mka
# ...and **7.1 Opus**, which is his largest film's soundtrack in miniature: five
# Opus streams, three coupled, and the widest layout the fold has a table for.
# One frequency per channel, four of the eight carrying anything, so the whole
# downmix is checkable by picking those four out of the folded pair
# (`tests/audio_multi.rs`): 440 in FL is left only, 880 in FC is both sides at
# -3 dB, 1320 in the LFE must be *gone*, and 1760 in SR is right only. Get the
# Vorbis-to-film permutation wrong at this width and every one of those moves.
ffmpeg -y -f lavfi -i "sine=frequency=440:duration=3:sample_rate=48000" \
    -f lavfi -i "sine=frequency=880:duration=3:sample_rate=48000" \
    -f lavfi -i "sine=frequency=1320:duration=3:sample_rate=48000" \
    -f lavfi -i "sine=frequency=1760:duration=3:sample_rate=48000" \
    -f lavfi -i "anullsrc=r=48000:cl=quad:d=3" \
    -filter_complex "[0:a][1:a][2:a][3:a][4:a]join=inputs=5:channel_layout=7.1:map=0.0-FL|4.0-FR|1.0-FC|2.0-LFE|4.1-BL|4.2-BR|4.3-SL|3.0-SR[a]" \
    -map "[a]" -c:a libopus -b:a 320k assets/test_opus_71.mka
# ...and standalone AC-3 in an `.mka`: the same syntax `test_ac3.mkv` carries,
# with no picture at all, which is DEBT #64's shape -- an AC-3 soundtrack
# ripped to its own Matroska file rather than left beside a video track. Read
# through the same [`MkvAc3Track`] door `.mkv` uses (`crate::demux::is_matroska`
# admits `.mka`), not symphonia, which has no AC-3 decoder.
ffmpeg -y -f lavfi -i "sine=frequency=440:duration=2:sample_rate=44100" \
    -c:a ac3 -b:a 192k -ac 2 assets/test_ac3.mka
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
# A file long enough to have a *cue table*, whose audio says where in it you
# are: a 30 s chirp from 200 Hz rising 160 Hz a second, so the frequency of any
# quarter second of it names the second it came from. That is what makes a seek
# landing measurable without a reference decode (`tests/audio_seek.rs`), and the
# length is the point -- symphonia's Matroska seek goes through the cues, and on
# a two-second fixture there are none to go wrong. Opus in an mkv because that
# is the film the desync was measured on, keyframes every five seconds because
# that is what writes a cue point per cluster.
ffmpeg -y -f lavfi -i "testsrc2=size=320x180:rate=24:duration=30" \
    -f lavfi -i "aevalsrc=sin(2*PI*(200*t+80*t*t)):s=48000:d=30" \
    -map 0:v -map 1:a -c:v libx264 -profile:v baseline -pix_fmt yuv420p -g 120 \
    -c:a libopus -b:a 128k assets/test_seek_chirp.mkv
# The lopsided container: 2 s of picture under 60 s of sound, which is what a
# still with a song over it or a botched remux looks like from the outside. The
# point is the *denominator* -- a file 60 s long whose video track is 2 s long
# reports thirty times its real byte rate if the bitrate row divides by the
# picture rather than by the file (`crates/engine/tests/bitrate.rs`). Nothing
# else here has this shape, which is exactly why it is here.
ffmpeg -y -f lavfi -i "testsrc2=size=320x240:rate=30:duration=2" \
    -f lavfi -i "sine=frequency=440:duration=60" \
    -map 0:v -map 1:a -c:v libx264 -profile:v baseline -pix_fmt yuv420p \
    -c:a aac -b:a 64k assets/test_short_video_long_audio.mp4
# A song wearing a film's extension: AAC audio, no video track, muxed into an
# `.mp4` -- the shape a phone's voice memo or a stripped remux takes, and the
# one `crate::is_audio` cannot see (`tests/audio_files.rs`).
ffmpeg -y -f lavfi -i "sine=frequency=440:duration=3" \
    -c:a aac -b:a 128k assets/test_audio_only.mp4

echo "fixtures written to assets/"
