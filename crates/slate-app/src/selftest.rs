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
    if std::env::args().any(|a| a == "--party-relay") {
        return party_relay();
    }
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

/// `slate --self-test --party-relay`: can this machine reach the relay a party would use (the
/// default iroh relays, or the one set under Settings → Party)? Prints one line, exits 0 / 1.
#[cfg(feature = "party")]
fn party_relay() -> i32 {
    let relay = crate::uiprefs::UiPrefs::load().party_relay;
    let relay = Some(relay.as_str()).filter(|r| !r.trim().is_empty());
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            println!("FAIL party relay: could not start: {e}");
            return 1;
        }
    };
    let result = rt.block_on(slate_core::party::net::check_relay(
        relay,
        std::time::Duration::from_secs(15),
    ));
    match result {
        Ok(r) => {
            println!(
                "ok   party relay reachable: {} (connected to {} in {} ms)",
                r.which, r.home, r.millis
            );
            0
        }
        Err(e) => {
            println!("FAIL party relay: {e}");
            1
        }
    }
}

#[cfg(not(feature = "party"))]
fn party_relay() -> i32 {
    println!("FAIL party relay: this build of Slate has no listening party");
    1
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
