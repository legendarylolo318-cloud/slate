//! Runs the real audio worker through a crossfade with two short generated tones at a very low
//! volume. It makes sound (a quiet beep for a few seconds), so it only runs when asked:
//! `cargo test -p slate-core --test crossfade_engine -- --ignored`.

use std::sync::Arc;
use std::time::Duration;

use slate_core::audio::AudioPlayer;
use slate_core::signal_path::SignalPathTracker;
use slate_core::{AppHandle, Events};

fn tone(path: &std::path::Path, freq: u32) {
    use gst::prelude::*;
    use gstreamer as gst;
    gst::init().expect("gst");
    let pipeline = gst::parse::launch(&format!(
        "audiotestsrc num-buffers=300 freq={freq} volume=0.2 ! audioconvert ! audio/x-raw,rate=44100,channels=2 ! wavenc ! filesink location={}",
        path.display()
    ))
    .expect("tone pipeline");
    pipeline.set_state(gst::State::Playing).expect("play");
    let bus = pipeline.bus().expect("bus");
    let _ = bus.timed_pop_filtered(
        gst::ClockTime::from_seconds(10),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    );
    pipeline.set_state(gst::State::Null).expect("null");
}

#[test]
#[ignore = "makes a quiet sound"]
fn a_fade_swaps_the_player_and_ends_cleanly() {
    let dir = std::env::temp_dir().join(format!("slate-fade-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let (a, b) = (dir.join("a.wav"), dir.join("b.wav"));
    tone(&a, 440);
    tone(&b, 660);

    let app = AppHandle::new(Events::null());
    let signal = Arc::new(SignalPathTracker::new(app.clone()));
    let player = AudioPlayer::new(app, Arc::clone(&signal), Default::default());
    player.set_volume(0.05).expect("volume");
    player
        .play_url(&format!("file://{}", a.display()), None)
        .expect("play a");
    std::thread::sleep(Duration::from_millis(1200));
    let before = player.get_position().expect("position");
    assert!(before > 0.5, "track A is playing: {before}");

    player.arm_crossfade(1.5);
    player
        .play_url(&format!("file://{}", b.display()), None)
        .expect("fade into b");
    assert!(player.is_crossfading());
    assert!(
        signal.snapshot().crossfading,
        "the signal path reads MODIFIED during a fade"
    );
    // Position now belongs to the incoming track, which just started.
    let during = player.get_position().expect("position during");
    assert!(
        during < before,
        "position follows the new track: {during} vs {before}"
    );

    std::thread::sleep(Duration::from_millis(2200));
    assert!(!player.is_crossfading(), "the fade ended");
    assert!(!signal.snapshot().crossfading);
    let after = player.get_position().expect("position after");
    assert!(
        after > during,
        "the new track kept playing: {after} vs {during}"
    );

    // Skipping mid-fade ends it at once.
    player.arm_crossfade(3.0);
    player
        .play_url(&format!("file://{}", a.display()), None)
        .expect("fade again");
    assert!(player.is_crossfading());
    player
        .play_url(&format!("file://{}", b.display()), None)
        .expect("plain start ends the fade");
    assert!(!player.is_crossfading());
    player.stop().expect("stop");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn nothing_is_armed_or_fading_by_default() {
    let app = AppHandle::new(Events::null());
    let signal = Arc::new(SignalPathTracker::new(app.clone()));
    let player = AudioPlayer::new(app, signal, Default::default());
    assert!(!player.is_crossfading());
    assert_eq!(player.crossfade_settings().secs, 0);
    player.set_crossfade(99, true);
    assert_eq!(
        player.crossfade_settings().secs,
        slate_core::crossfade::MAX_SECS
    );
}
