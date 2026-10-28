//! MPRIS 2 server (Linux). Runs a current-thread runtime on its own thread
//! because the D-Bus objects are `!Send`.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use mpris_server::{LoopStatus, Metadata, PlaybackStatus, Player, Time, TrackId};
use sone_core::media::{MediaCommand, MediaSink};
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

use crate::control::{Control, ControlFn, PositionFn};

pub struct MprisSink {
    tx: mpsc::UnboundedSender<MediaCommand>,
}

impl MediaSink for MprisSink {
    fn send(&self, cmd: MediaCommand) {
        let _ = self.tx.send(cmd);
    }
}

pub fn start(control: ControlFn, position: PositionFn) -> Arc<MprisSink> {
    let (tx, rx) = mpsc::unbounded_channel();
    let spawned = std::thread::Builder::new()
        .name("sone-mpris".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("MPRIS runtime failed: {e}");
                    return;
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, serve(rx, control, position));
        });
    if let Err(e) = spawned {
        tracing::error!("could not start MPRIS thread: {e}");
    }
    Arc::new(MprisSink { tx })
}

async fn serve(
    mut rx: mpsc::UnboundedReceiver<MediaCommand>,
    control: ControlFn,
    position: PositionFn,
) {
    // snapd's mpris interface rejects dotted names; everything else keeps the
    // reverse-DNS app id.
    let (bus_name, desktop_entry) = if std::env::var("SNAP").is_ok() {
        ("sone", "sone_sone")
    } else {
        ("io.github.lullabyX.sone", "io.github.lullabyX.sone")
    };
    let player = match Player::builder(bus_name)
        .can_play(true)
        .can_pause(true)
        .can_go_next(true)
        .can_go_previous(true)
        .can_seek(true)
        .can_control(true)
        .can_quit(true)
        .can_raise(true)
        .has_track_list(false)
        .supported_uri_schemes(vec!["tidal".to_string()])
        .rate(1.0)
        .minimum_rate(1.0)
        .maximum_rate(1.0)
        .shuffle(false)
        .loop_status(LoopStatus::None)
        .build()
        .await
    {
        Ok(p) => Rc::new(p),
        Err(e) => {
            tracing::error!("MPRIS player failed to start: {e}");
            return;
        }
    };
    let _ = player.set_identity("SONE").await;
    let _ = player.set_desktop_entry(desktop_entry).await;

    let c = control.clone();
    player.connect_play_pause(move |_| c(Control::PlayPause));
    let c = control.clone();
    player.connect_play(move |_| c(Control::Play));
    let c = control.clone();
    player.connect_pause(move |_| c(Control::Pause));
    let c = control.clone();
    player.connect_stop(move |_| c(Control::Stop));
    let c = control.clone();
    player.connect_next(move |_| c(Control::Next));
    let c = control.clone();
    player.connect_previous(move |_| c(Control::Previous));
    let c = control.clone();
    player.connect_seek(move |_, off| {
        c(Control::SeekRelative(off.as_micros() as f64 / 1_000_000.0));
    });
    let c = control.clone();
    player.connect_set_position(move |_, _id, pos| {
        c(Control::SetPosition(pos.as_micros() as f64 / 1_000_000.0));
    });
    let c = control.clone();
    player.connect_set_volume(move |_, v| c(Control::SetVolume(v)));
    let c = control.clone();
    player.connect_set_shuffle(move |_, s| c(Control::SetShuffle(s)));
    let c = control.clone();
    player.connect_set_loop_status(move |_, st| {
        c(Control::SetLoop(match st {
            LoopStatus::None => 0,
            LoopStatus::Playlist => 1,
            LoopStatus::Track => 2,
        }));
    });
    let c = control.clone();
    player.connect_raise(move |_| c(Control::Raise));
    let c = control.clone();
    player.connect_quit(move |_| c(Control::Quit));

    tokio::task::spawn_local(player.run());

    // The spec's `Position` is not signalled; clients extrapolate from the last
    // anchor, so refresh it once a second while playing.
    let ticking = Rc::clone(&player);
    tokio::task::spawn_local(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        tick.tick().await;
        loop {
            tick.tick().await;
            if ticking.playback_status() != PlaybackStatus::Playing {
                continue;
            }
            if let Some(secs) = position() {
                ticking.set_position(Time::from_micros((secs as f64 * 1_000_000.0) as i64));
            }
        }
    });
    tracing::info!("MPRIS D-Bus server started");

    while let Some(cmd) = rx.recv().await {
        match cmd {
            MediaCommand::SetMetadata {
                track_id,
                title,
                artist,
                album,
                art_url,
                duration_secs,
                url,
                album_artist,
                track_number,
                disc_number,
                content_created,
                user_rating,
            } => {
                let mut m = Metadata::new();
                if let Ok(id) =
                    TrackId::try_from(format!("/org/mpris/MediaPlayer2/Track/{track_id}").as_str())
                {
                    m.set_trackid(Some(id));
                }
                m.set_title(Some(&title));
                m.set_artist(Some([&artist]));
                m.set_album(Some(&album));
                if !art_url.is_empty() {
                    m.set_art_url(Some(art_url));
                }
                m.set_length(Some(Time::from_micros(
                    (duration_secs * 1_000_000.0) as i64,
                )));
                if let Some(u) = url.filter(|s| !s.is_empty()) {
                    m.set_url(Some(u));
                }
                if let Some(a) = album_artist.filter(|s| !s.is_empty()) {
                    m.set_album_artist(Some([a]));
                }
                if let Some(n) = track_number {
                    m.set_track_number(Some(n as i32));
                }
                if let Some(n) = disc_number {
                    m.set_disc_number(Some(n as i32));
                }
                if let Some(d) = content_created.filter(|s| !s.is_empty()) {
                    m.set_content_created(Some(d));
                }
                if let Some(r) = user_rating {
                    m.set_user_rating(Some(r));
                }
                let _ = player.set_metadata(m).await;
                player.set_position(Time::ZERO);
            }
            MediaCommand::SetPlaybackStatus { is_playing } => {
                let st = if is_playing {
                    PlaybackStatus::Playing
                } else {
                    PlaybackStatus::Paused
                };
                let _ = player.set_playback_status(st).await;
            }
            MediaCommand::SetVolume { volume } => {
                let _ = player.set_volume(volume).await;
            }
            MediaCommand::Seeked { position_secs } => {
                let t = Time::from_micros((position_secs * 1_000_000.0) as i64);
                player.set_position(t);
                let _ = player.seeked(t).await;
            }
            MediaCommand::SetShuffle { enabled } => {
                let _ = player.set_shuffle(enabled).await;
            }
            MediaCommand::SetLoopStatus { mode } => {
                let st = match mode {
                    1 => LoopStatus::Playlist,
                    2 => LoopStatus::Track,
                    _ => LoopStatus::None,
                };
                let _ = player.set_loop_status(st).await;
            }
            // Fullscreen is not a Slint-level feature yet.
            MediaCommand::SetFullscreen { .. } => {}
            MediaCommand::Stop => {
                let _ = player.set_playback_status(PlaybackStatus::Stopped).await;
            }
        }
    }
}
