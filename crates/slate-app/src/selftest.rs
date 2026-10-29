//! `slate --self-test`: checks a packaged build can find its GStreamer plugins
//! and make an HTTPS request through them, then exits. Used by the release
//! smoke tests; prints one line per check and exits non-zero on any failure.

use gst::prelude::*;
use gstreamer as gst;

/// Elements playback needs on every platform.
const REQUIRED: &[&str] = &[
    "uridecodebin",
    "souphttpsrc",
    "dashdemux",
    "typefind",
    "queue",
    "concat",
    "audioconvert",
    "audioresample",
    "volume",
    "autoaudiosink",
    "appsink",
    "flacparse",
    "videoconvert",
];

#[cfg(windows)]
const PLATFORM: &[&str] = &["wasapi2sink", "wasapisink"];
#[cfg(target_os = "linux")]
const PLATFORM: &[&str] = &["alsasink"];
#[cfg(not(any(windows, target_os = "linux")))]
const PLATFORM: &[&str] = &[];

pub fn run() -> i32 {
    let mut failed = 0;
    let mut report = |ok: bool, what: &str| {
        println!("{} {what}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            failed += 1;
        }
    };
    if let Err(e) = gst::init() {
        report(false, &format!("gst::init: {e}"));
        return 1;
    }
    let registry = gst::Registry::get();
    report(
        registry.plugins().len() > 10,
        &format!("{} plugins loaded", registry.plugins().len()),
    );
    for name in REQUIRED.iter().chain(PLATFORM) {
        report(gst::ElementFactory::find(name).is_some(), name);
    }
    // FLAC decode: one of the decoders TIDAL lossless needs.
    report(
        ["flacdec", "avdec_flac"]
            .iter()
            .any(|n| gst::ElementFactory::find(n).is_some()),
        "flac decoder",
    );
    report(
        ["avdec_aac", "faad", "fdkaacdec"]
            .iter()
            .any(|n| gst::ElementFactory::find(n).is_some()),
        "aac decoder",
    );
    if std::env::args().any(|a| a == "--network") {
        report(
            https_fetch().is_ok(),
            "HTTPS fetch through souphttpsrc (TLS backend)",
        );
    }
    i32::from(failed > 0)
}

fn https_fetch() -> Result<(), String> {
    let pipeline =
        gst::parse::launch("souphttpsrc name=src location=https://www.example.com/ ! fakesink")
            .map_err(|e| e.to_string())?;
    // Same CA handling as playback, so this also checks distributions whose
    // CA bundle is not where the bundled GnuTLS expects it.
    if let Some(src) = pipeline
        .downcast_ref::<gst::Bin>()
        .and_then(|b| b.by_name("src"))
    {
        slate_core::audio::apply_ca_bundle(&src);
    }
    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| e.to_string())?;
    let bus = pipeline.bus().ok_or("no bus")?;
    let result = match bus.timed_pop_filtered(
        gst::ClockTime::from_seconds(20),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    ) {
        Some(msg) => match msg.view() {
            gst::MessageView::Eos(_) => Ok(()),
            gst::MessageView::Error(e) => Err(format!("{} ({:?})", e.error(), e.debug())),
            _ => Err("unexpected message".into()),
        },
        None => Err("timed out".into()),
    };
    let _ = pipeline.set_state(gst::State::Null);
    if let Err(e) = &result {
        println!("     {e}");
    }
    result
}
