//! `probe_bitrate`: the number a properties card shows, and the two rules that
//! make it worth showing -- it is the file's real bytes over the file's real
//! seconds, and a field the container never stated is absent rather than zero.

use std::path::{Path, PathBuf};
use std::time::Instant;

use engine::{MediaBitrate, probe_bitrate};

fn asset(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(name)
}

/// What the fixture generator asked ffmpeg for, in seconds -- `-duration`/
/// `-t` in `scripts/gen_fixtures.sh`. The independent half of the check: the
/// probe derives its seconds from the container (frame count over frame rate,
/// or the audio header), and this derives them from nowhere but the script that
/// wrote the file.
const FIXTURES: &[(&str, f64)] = &[
    ("test_av.mp4", 5.0),
    ("test_av1.mkv", 2.0),
    ("test_hevc.mkv", 2.0),
    ("test_tone_48k.wav", 3.0),
];

/// Every fixture with a container to read, well-shaped or not. The two rules
/// below are held over all of them rather than over the tidy ones.
const CONTAINERS: &[&str] = &[
    "test_av.mp4",
    "test_av1.mkv",
    "test_hevc.mkv",
    "test_hevc10.mkv",
    "test_multiaudio.mp4",
    "test_multiaudio.mkv",
    "test_ac3.mkv",
    "test_eac3.mkv",
    "test_subs.mkv",
    "test_vp9.mp4",
    "test_h264.mkv",
    "test_tone_48k.wav",
    "test_short_video_long_audio.mp4",
];

/// No field is ever `Some(0)`: a zero read off a card is a measurement, and
/// "the header did not say" is not one. Held over every field of every fixture,
/// including the ones with no bitrate at all.
fn no_field_is_zero(name: &str, rate: MediaBitrate) {
    for (field, value) in [
        ("total", rate.total),
        ("video", rate.video),
        ("audio", rate.audio),
    ] {
        assert_ne!(value, Some(0), "{name}: {field} came back a fabricated zero");
    }
}

#[test]
fn a_breakdown_adds_up() {
    // The relation that makes the three numbers one line rather than three
    // unrelated ones: they are disjoint byte counts over a shared denominator,
    // so the parts cannot outweigh the whole and the whole cannot outweigh the
    // file. Asserted over every container fixture, not trusted to the
    // well-shaped ones -- a total *below* its own components is impossible, and
    // a total above the file's byte rate means the denominator is too small,
    // which is exactly what dividing by the video track used to do.
    for &name in CONTAINERS {
        let path = asset(name);
        let bytes = std::fs::metadata(&path).expect(name).len();
        let rate = probe_bitrate(&path);
        no_field_is_zero(name, rate);
        let total = rate.total.unwrap_or_else(|| panic!("{name}: no total"));
        let parts = rate.video.unwrap_or(0) + rate.audio.unwrap_or(0);
        assert!(
            total >= parts,
            "{name}: total {total} is under its own parts {parts}"
        );
        // The ceiling: no file plays in less than no time, so no rate can beat
        // the whole file delivered over the shortest plausible duration. One
        // second is that floor -- every fixture here is longer.
        assert!(
            total <= bytes * 8,
            "{name}: total {total} exceeds the file's own {} bits",
            bytes * 8
        );
    }
}

#[test]
fn a_long_soundtrack_over_a_short_picture_divides_by_the_file() {
    // The lopsided container (`gen_fixtures.sh`): 2 s of video under 60 s of
    // audio. Dividing 574 222 bytes by the *picture's* 2 s claimed 4 181 048 bps
    // -- thirty times the truth, and a "total" smaller than its own components
    // when written out. `ffprobe -show_entries format=bit_rate` says 76 562, and
    // the container states its own 60 s in the `mvhd` for anyone who asks.
    let name = "test_short_video_long_audio.mp4";
    let path = asset(name);
    let bytes = std::fs::metadata(&path).expect(name).len();
    let rate = probe_bitrate(&path);
    no_field_is_zero(name, rate);

    let total = rate.total.expect("no total");
    let expected = bytes as f64 * 8.0 / 60.0;
    let error = (total as f64 - expected).abs() / expected;
    assert!(
        error < 0.05,
        "{name}: total {total} is {:.1} % off the file over its own 60 s ({expected:.0})",
        error * 100.0
    );
    // The picture is a fifth of the sound here, which is the whole point: a
    // component measured over the video's 2 s would come out above the total.
    let video = rate.video.expect("no video rate");
    let audio = rate.audio.expect("no audio rate");
    assert!(video < audio, "{name}: video {video} >= audio {audio}");
    assert!(
        video + audio <= total,
        "{name}: parts {} beat the total {total}",
        video + audio
    );
}

#[test]
fn total_is_the_file_over_its_seconds() {
    for &(name, secs) in FIXTURES {
        let path = asset(name);
        let bytes = std::fs::metadata(&path).expect(name).len();
        let expected = bytes as f64 * 8.0 / secs;

        let rate = probe_bitrate(&path);
        no_field_is_zero(name, rate);
        let total = rate.total.unwrap_or_else(|| panic!("{name}: no total"));

        let error = (total as f64 - expected).abs() / expected;
        assert!(
            error < 0.05,
            "{name}: total {total} bps is {:.1} % off {expected:.0} bps \
             ({bytes} bytes over {secs} s)",
            error * 100.0
        );
    }
}

#[test]
fn a_stream_the_container_states_is_stated_and_no_stream_is_invented() {
    // Every A/V fixture here has a picture, so the video row is a claim the
    // container can always answer -- an mkv out of its own block index, an mp4
    // out of its sample table. It must be under the whole file's rate too: a
    // track cannot cost more than the file it sits in.
    for &(name, secs) in FIXTURES {
        let rate = probe_bitrate(&asset(name));
        let total = rate.total.unwrap_or_else(|| panic!("{name}: no total"));
        if name.ends_with(".wav") {
            // One stream, no picture: the sound *is* the file, and there is no
            // video row to state.
            assert_eq!(rate.video, None, "{name}: a wav has no picture");
            assert_eq!(rate.audio, Some(total), "{name}");
            continue;
        }
        let video = rate.video.unwrap_or_else(|| panic!("{name}: no video rate"));
        assert!(video < total, "{name}: video {video} >= file {total}");
        // Sanity on the units: 5 s of 1280x720 is neither 2 kbps nor 2 Gbps.
        assert!(
            (10_000..100_000_000).contains(&video),
            "{name}: video {video} bps is not a plausible rate for {secs} s of 720p"
        );
        if let Some(audio) = rate.audio {
            assert!(audio < total, "{name}: audio {audio} >= file {total}");
        }
    }
}

#[test]
fn a_matroska_states_its_sound_too_and_its_picture_is_unchanged() {
    // Both mkv fixtures carry `-c:a aac -b:a 128k` (`gen_fixtures.sh:158,223`),
    // so an absent audio row here is a missing measurement, not a silent file --
    // and his library is overwhelmingly Matroska, so this is the common case.
    //
    // The video numbers are pinned so that adding the sound track to the walk
    // cannot move the picture. The numerator is the one `ffprobe
    // -select_streams v -show_entries packet=size` sums independently -- 716 834
    // bytes for test_av1.mkv. The denominator is `Info.Duration`, **2.023 s**,
    // not the picture's own 2.000 s: the AAC track outlasts the last frame by
    // 23 ms and the file really is that long (`ffprobe format=duration` agrees).
    // These were 2_867_336 / 2_086_260 while the divisor was the video track,
    // i.e. 1.1 % high, which is the same defect the lopsided fixture shows at
    // 30x.
    for (name, video_bps) in [("test_av1.mkv", 2_834_736), ("test_hevc.mkv", 2_062_540)] {
        let rate = probe_bitrate(&asset(name));
        no_field_is_zero(name, rate);
        assert_eq!(rate.video, Some(video_bps), "{name}: picture moved");
        let audio = rate
            .audio
            .unwrap_or_else(|| panic!("{name}: no audio rate, and it has a 128k AAC track"));
        assert!(
            (100_000..170_000).contains(&audio),
            "{name}: audio {audio} bps, asked for 128000"
        );
    }
}

#[test]
fn the_mp4_rows_are_what_the_encoder_was_asked_for() {
    // `gen_fixtures.sh:18` writes test_av.mp4's sound at `-b:a 128k`. Nothing
    // reads `esds.avg_bitrate` any more -- the sample table is what the track
    // actually spends -- so this is the check that walking the `stsz` lands
    // where the declared number did. `ffprobe` puts this track at 132 311 and
    // the picture at 3 119 464, both over the same 5 s the `mvhd` states.
    let rate = probe_bitrate(&asset("test_av.mp4"));
    assert_eq!(rate.video, Some(3_119_464), "test_av.mp4: picture");
    let audio = rate.audio.expect("test_av.mp4: no audio rate");
    assert!(
        (120_000..140_000).contains(&audio),
        "test_av.mp4: audio {audio} bps, asked for 128000"
    );
}

#[test]
fn a_dual_audio_file_names_the_track_it_would_play_and_says_there_are_others() {
    // `test_multiaudio.mp4` is the shape a real dual-audio mp4 has:
    // an AAC stereo track and a fatter AC-3 one in the same file. Whichever the
    // bitrate row names, it names *one* of three, and the danger is calling a
    // 0.13 Mb/s stereo track "sound" on a file whose selling point is the AC-3.
    //
    // The pick is the first audio track in file order, which is the same rule
    // `AudioSession::probe_streams` numbers stream 0 by and the same one
    // playback and export open. Asserted against `probe_streams` itself rather
    // than against a constant, so the two can never drift apart silently: the
    // bitrate row and the audio row of one card describe one track.
    let name = "test_multiaudio.mp4";
    let path = asset(name);
    let streams = engine::AudioSession::probe_streams(&path).expect(name);
    assert!(
        streams.len() > 1,
        "{name}: the fixture stopped being multi-audio, this test is now vacuous"
    );
    let rate = probe_bitrate(&path);
    let audio = rate.audio.expect("no audio rate");

    // Stream 0 is the 44.1k AAC stereo one (`gen_fixtures.sh:41`); the AC-3 at
    // 192k is stream 2. Naming the wrong one would land near 192000.
    assert_eq!(streams[0].index, 0, "{name}: stream order moved");
    assert_eq!(streams[0].codec, "aac", "{name}: stream 0 is not the AAC one");
    assert!(
        (110_000..145_000).contains(&audio),
        "{name}: audio {audio} bps is not stream 0's ~127 kbps AAC -- the row \
         and the audio row now name different tracks"
    );
    // And the signal that it is one of several: the caller has the count in
    // hand from the same probe it already runs, so a card can say so. This
    // asserts the count is *reachable*, which is what makes the single number
    // honest rather than a silent pick.
    assert_eq!(streams.len(), 3, "{name}");
}

#[test]
fn a_still_states_no_rate_at_all() {
    // A still has no playing time of its own -- its length on the timeline is
    // whatever the clip is stretched to -- so there is no per-second anything
    // to divide 1507 bytes by. All three fields absent is the honest answer;
    // any number here would be invented.
    let rate = probe_bitrate(&asset("test_still.png"));
    no_field_is_zero("test_still.png", rate);
    assert_eq!(rate, MediaBitrate::default(), "test_still.png");
}

#[test]
fn a_file_that_will_not_open_states_no_rate_either() {
    // Not an error and not a zero: a probe run over a whole library meets these
    // and the card simply has no row.
    let rate = probe_bitrate(Path::new("/nonexistent/nothing.mp4"));
    assert_eq!(rate, MediaBitrate::default());
    no_field_is_zero("missing", rate);
}

#[test]
fn no_decoder_is_opened() {
    // The whole point of the probe: it is cheap enough to run over every file a
    // library lists. Decoding one frame of 10-bit HEVC costs far more than this
    // budget, so a wall clock is what proves no decoder was opened -- 2 s of
    // 720p Main 10 through the header path is single-digit milliseconds.
    let path = asset("test_hevc10.mkv");
    let started = Instant::now();
    let rate = probe_bitrate(&path);
    let elapsed = started.elapsed();
    no_field_is_zero("test_hevc10.mkv", rate);
    assert!(rate.total.is_some(), "test_hevc10.mkv: no total");
    assert!(
        elapsed.as_millis() < 50,
        "probe_bitrate took {elapsed:?}, which is decode territory"
    );
}
