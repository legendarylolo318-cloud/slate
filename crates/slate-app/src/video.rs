//! Music-video playback: a GStreamer `playbin3` whose video sink is an RGBA
//! appsink; every frame becomes a `slint::Image` on the UI thread.
//!
//! The audio engine in `slate-core` is untouched; starting a video pauses it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use slate_core::commands::playback;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use crate::backend::Backend;
use crate::images::format_time;

/// Frames wider than this are scaled down before the copy.
const MAX_WIDTH: i32 = 1280;
const MAX_HEIGHT: i32 = 720;

pub struct VideoSession {
    pipeline: gst::Element,
}

impl Drop for VideoSession {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

fn build(uri: &str, backend: &Backend) -> Result<gst::Element, String> {
    gst::init().map_err(|e| e.to_string())?;
    let sink_bin = gst::parse::bin_from_description(
        &format!(
            "videoconvert ! videoscale ! video/x-raw,format=RGBA,width=[1,{MAX_WIDTH}],height=[1,{MAX_HEIGHT}] ! appsink name=frames"
        ),
        true,
    )
    .map_err(|e| e.to_string())?;
    let appsink = sink_bin
        .by_name("frames")
        .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
        .ok_or("video sink unavailable")?;
    appsink.set_property("sync", true);
    appsink.set_property("max-buffers", 1u32);
    appsink.set_property("drop", true);

    let pending = Arc::new(AtomicBool::new(false));
    let ui = backend.ui.clone();
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                // A frame is already waiting for the UI thread: drop this one.
                if pending.swap(true, Ordering::AcqRel) {
                    return Ok(gst::FlowSuccess::Ok);
                }
                let (Some(buffer), Some(caps)) = (sample.buffer(), sample.caps()) else {
                    pending.store(false, Ordering::Release);
                    return Ok(gst::FlowSuccess::Ok);
                };
                let dims = caps.structure(0).and_then(|s| {
                    Some((s.get::<i32>("width").ok()?, s.get::<i32>("height").ok()?))
                });
                let (Some((w, h)), Ok(map)) = (dims, buffer.map_readable()) else {
                    pending.store(false, Ordering::Release);
                    return Ok(gst::FlowSuccess::Ok);
                };
                let (w, h) = (w as u32, h as u32);
                let mut px = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
                let dst = px.make_mut_bytes();
                let n = dst.len().min(map.len());
                dst[..n].copy_from_slice(&map[..n]);
                let flag = Arc::clone(&pending);
                let queued = ui.upgrade_in_event_loop(move |win| {
                    win.set_video_frame(Image::from_rgba8(px));
                    win.set_video_status("".into());
                    flag.store(false, Ordering::Release);
                });
                if queued.is_err() {
                    pending.store(false, Ordering::Release);
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );

    let pipeline = gst::ElementFactory::make("playbin3")
        .property("uri", uri)
        .property("video-sink", &sink_bin)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(pipeline)
}

impl Backend {
    pub fn play_video(&self, id: u64) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            // Video goes straight to the network from GStreamer, outside the
            // proxy-aware HTTP client, so it must not run while a proxy is set.
            if state.load_settings().is_some_and(|s| s.proxy.enabled) {
                this.ui(|w| {
                    w.set_error("Music videos are unavailable while a proxy is enabled.".into())
                });
                return;
            }
            let meta = playback::get_video_metadata(&state, id).await;
            let info = playback::get_video_stream_info(&state, id, None).await;
            let (title, url) = match (meta, info) {
                (Ok(m), Ok(i)) => (m.title, i.url),
                (_, Err(e)) | (Err(e), _) => {
                    let msg = format!("Could not load video: {}", e.log_safe());
                    this.ui(move |w| w.set_error(msg.into()));
                    return;
                }
            };
            // One thing at a time: pause the audio engine.
            let _ = playback::pause_track(&state).await;
            this.shared_set_playing(false);
            this.ui(move |w| {
                w.set_video_title(title.into());
                w.set_video_frame(Image::default());
                w.set_video_status("Loading video…".into());
                w.set_video_playing(true);
                w.set_video_progress(0.0);
                w.set_video_open(true);
            });
            let backend = this.clone();
            let built = tokio::task::spawn_blocking(move || {
                let pipeline = build(&url, &backend)?;
                pipeline
                    .set_state(gst::State::Playing)
                    .map_err(|e| e.to_string())?;
                Ok::<_, String>(pipeline)
            })
            .await;
            match built {
                Ok(Ok(pipeline)) => {
                    if let Ok(mut s) = this.video.lock() {
                        *s = Some(VideoSession { pipeline });
                    }
                    this.spawn_video_ticker();
                }
                Ok(Err(e)) => {
                    this.ui(move |w| w.set_video_status(format!("Video failed: {e}").into()))
                }
                Err(e) => this.ui(move |w| w.set_video_status(format!("Video failed: {e}").into())),
            }
        });
    }

    fn spawn_video_ticker(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(500));
            loop {
                tick.tick().await;
                let (pos, dur, eos) = {
                    let Ok(guard) = this.video.lock() else { return };
                    let Some(s) = guard.as_ref() else { return };
                    let pos = s.pipeline.query_position::<gst::ClockTime>();
                    let dur = s.pipeline.query_duration::<gst::ClockTime>();
                    let eos = s
                        .pipeline
                        .bus()
                        .and_then(|b| b.pop_filtered(&[gst::MessageType::Eos]))
                        .is_some();
                    (pos, dur, eos)
                };
                let secs = |t: Option<gst::ClockTime>| t.map_or(0.0, |t| t.seconds() as f32);
                let (p, d) = (secs(pos), secs(dur));
                this.ui(move |w| {
                    w.set_video_progress(if d > 0.0 {
                        (p / d).clamp(0.0, 1.0)
                    } else {
                        0.0
                    });
                    w.set_video_position_text(format_time(p).into());
                    w.set_video_duration_text(format_time(d).into());
                });
                if eos {
                    this.close_video();
                    this.ui(|w| w.set_video_open(false));
                    return;
                }
            }
        });
    }

    pub fn video_toggle(&self) {
        let playing = {
            let Ok(guard) = self.video.lock() else { return };
            let Some(s) = guard.as_ref() else { return };
            let (_, current, _) = s.pipeline.state(gst::ClockTime::ZERO);
            let now_playing = current == gst::State::Playing;
            let _ = s.pipeline.set_state(if now_playing {
                gst::State::Paused
            } else {
                gst::State::Playing
            });
            !now_playing
        };
        self.ui(move |w| w.set_video_playing(playing));
    }

    pub fn video_seek(&self, fraction: f32) {
        let Ok(guard) = self.video.lock() else { return };
        let Some(s) = guard.as_ref() else { return };
        if let Some(d) = s.pipeline.query_duration::<gst::ClockTime>() {
            let target = gst::ClockTime::from_nseconds(
                (d.nseconds() as f64 * f64::from(fraction.clamp(0.0, 1.0))) as u64,
            );
            let _ = s
                .pipeline
                .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, target);
        }
    }

    pub fn close_video(&self) {
        if let Ok(mut s) = self.video.lock() {
            s.take();
        }
    }
}

pub type SharedVideo = Arc<Mutex<Option<VideoSession>>>;
