use crate::discord_ipc::SlateDiscordClient;
use discord_rich_presence::error::Error;
use discord_rich_presence::{activity, DiscordIpc};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Slate's Discord application; Discord shows its name ("Slate") in the
/// presence.
const APPLICATION_ID: &str = "1565205779366158388";

/// Slate logo drawn as the small badge over the album art's corner, served
/// from this repository rather than upstream's.
const SLATE_LOGO_URL: &str =
    "https://raw.githubusercontent.com/legendarylolo318-cloud/slate/main/assets/icons/128x128.png";

/// How often an idle Discord thread retries, or re-publishes to notice that
/// the connection has gone away.
const RECONNECT_INTERVAL: Duration = Duration::from_secs(30);

pub enum DiscordCommand {
    SetMetadata {
        title: String,
        artist: String,
        album: String,
        art_url: String,
        duration_secs: f64,
        url: String,
        quality_text: String,
        playlist: String,
        year: String,
        /// Do not show a presence for this track (private playlist, or hidden by the user).
        hidden: bool,
    },
    SetPlaying {
        is_playing: bool,
        position_secs: f64,
    },
    Stop,
    Seeked {
        position_secs: f64,
    },
    SetStatusText {
        text: String,
    },
    SetTemplates {
        details: String,
        state: String,
    },
    Connect,
    Disconnect,
}

pub struct DiscordHandle {
    tx: mpsc::Sender<DiscordCommand>,
}

#[derive(Default)]
struct CurrentActivity {
    title: String,
    artist: String,
    album: String,
    art_url: String,
    duration_secs: f64,
    url: String,
    quality_text: String,
    status_text: String,
    details_template: String,
    state_template: String,
    playlist: String,
    year: String,
    hidden: bool,
    is_playing: bool,
    play_start_epoch: i64,
}

impl DiscordHandle {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<DiscordCommand>();

        std::thread::spawn(move || {
            let mut client = SlateDiscordClient::new(APPLICATION_ID);

            let mut connected = false;
            let mut want_connected = false;
            // Set once a connect failure has been logged, cleared on the next
            // success, so the 30s tick reports state *transitions* rather than
            // repeating the same line twice a minute forever.
            let mut logged_failure = false;
            let mut current = CurrentActivity::default();

            // Try to establish or re-establish the IPC connection.
            // Always creates a fresh client to avoid stale socket issues.
            let try_connect = |client: &mut SlateDiscordClient,
                               connected: &mut bool,
                               logged_failure: &mut bool|
             -> bool {
                if *connected {
                    return true;
                }
                *client = SlateDiscordClient::new(APPLICATION_ID);
                match client.connect() {
                    Ok(()) => {
                        *connected = true;
                        *logged_failure = false;
                        log::info!("Discord Rich Presence connected");
                        true
                    }
                    Err(e) => {
                        if !*logged_failure {
                            *logged_failure = true;
                            // No socket at all just means Discord is not
                            // running, the normal state for most users.
                            if matches!(e, Error::IPCNotFound) {
                                log::debug!("Discord IPC socket not present");
                            } else {
                                log::warn!("Failed to connect Discord IPC: {e}");
                            }
                        }
                        false
                    }
                }
            };

            loop {
                // Only wake on a timer when there is something to retry or
                // re-verify; otherwise block so a disabled or idle integration
                // costs nothing.
                let cmd = if want_connected {
                    match rx.recv_timeout(RECONNECT_INTERVAL) {
                        Ok(cmd) => cmd,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if !connected {
                                // Discord may have started after we did.
                                if try_connect(&mut client, &mut connected, &mut logged_failure)
                                    && current.is_playing
                                    && !current.title.is_empty()
                                    && publish_activity(&mut client, &current).is_err()
                                {
                                    client.close().ok();
                                    connected = false;
                                }
                            } else if current.is_playing && !current.title.is_empty() {
                                // Re-publishing is the only way we learn the
                                // socket died: a write to a departed Discord
                                // fails. One update per 30s is well inside
                                // every documented SET_ACTIVITY rate limit.
                                if publish_activity(&mut client, &current).is_err() {
                                    client.close().ok();
                                    connected = false;
                                }
                            }
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                } else {
                    match rx.recv() {
                        Ok(cmd) => cmd,
                        Err(_) => break,
                    }
                };

                match cmd {
                    DiscordCommand::Connect => {
                        want_connected = true;
                        try_connect(&mut client, &mut connected, &mut logged_failure);
                        if connected
                            && current.is_playing
                            && !current.title.is_empty()
                            && publish_activity(&mut client, &current).is_err()
                        {
                            client.close().ok();
                            connected = false;
                        }
                    }
                    DiscordCommand::Disconnect => {
                        want_connected = false;
                        if connected {
                            client.clear_activity().ok();
                            client.close().ok();
                            connected = false;
                            log::info!("Discord Rich Presence disconnected");
                        }
                    }
                    DiscordCommand::SetMetadata {
                        title,
                        artist,
                        album,
                        art_url,
                        duration_secs,
                        url,
                        quality_text,
                        playlist,
                        year,
                        hidden,
                    } => {
                        current.playlist = playlist;
                        current.year = year;
                        current.hidden = hidden;
                        current.title = title;
                        current.artist = artist;
                        current.album = album;
                        current.art_url = art_url;
                        current.duration_secs = duration_secs;
                        current.url = url;
                        current.quality_text = quality_text;

                        if current.is_playing {
                            current.play_start_epoch = now_epoch_secs();
                        }

                        if want_connected {
                            try_connect(&mut client, &mut connected, &mut logged_failure);
                            if connected && publish_activity(&mut client, &current).is_err() {
                                client.close().ok();
                                connected = false;
                            }
                        }
                    }
                    DiscordCommand::SetPlaying {
                        is_playing: playing,
                        position_secs,
                    } => {
                        current.is_playing = playing;

                        if playing {
                            current.play_start_epoch = now_epoch_secs() - position_secs as i64;
                        }

                        if want_connected {
                            try_connect(&mut client, &mut connected, &mut logged_failure);
                            if connected {
                                let failed = if !playing {
                                    client.clear_activity().is_err()
                                } else {
                                    publish_activity(&mut client, &current).is_err()
                                };
                                if failed {
                                    client.close().ok();
                                    connected = false;
                                }
                            }
                        }
                    }
                    DiscordCommand::SetTemplates { details, state } => {
                        current.details_template = details;
                        current.state_template = state;
                        if want_connected
                            && connected
                            && current.is_playing
                            && !current.title.is_empty()
                            && publish_activity(&mut client, &current).is_err()
                        {
                            client.close().ok();
                            connected = false;
                        }
                    }
                    DiscordCommand::SetStatusText { text } => {
                        current.status_text = text;
                        if want_connected
                            && connected
                            && current.is_playing
                            && !current.title.is_empty()
                            && publish_activity(&mut client, &current).is_err()
                        {
                            client.close().ok();
                            connected = false;
                        }
                    }
                    DiscordCommand::Stop => {
                        current.title.clear();
                    }
                    DiscordCommand::Seeked { position_secs } => {
                        if current.is_playing {
                            current.play_start_epoch = now_epoch_secs() - position_secs as i64;
                            if want_connected
                                && connected
                                && publish_activity(&mut client, &current).is_err()
                            {
                                client.close().ok();
                                connected = false;
                            }
                        }
                    }
                }
            }

            // Channel closed — clean up
            if connected {
                client.clear_activity().ok();
                client.close().ok();
            }
        });

        Self { tx }
    }

    pub fn send(&self, cmd: DiscordCommand) {
        self.tx.send(cmd).ok();
    }
}

impl Default for DiscordHandle {
    fn default() -> Self {
        Self::new()
    }
}

fn now_epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn publish_activity(client: &mut SlateDiscordClient, current: &CurrentActivity) -> Result<(), ()> {
    if current.hidden {
        // Playing from a playlist the user keeps off Discord: show nothing at all.
        return client.clear_activity().map_err(|_| ());
    }
    let fields = crate::discord_template::Fields {
        title: &current.title,
        artist: &current.artist,
        album: &current.album,
        quality: &current.quality_text,
        playlist: &current.playlist,
        year: &current.year,
    };
    let (details, state_text) =
        crate::discord_template::lines(&current.details_template, &current.state_template, &fields);
    let details_text = details.unwrap_or_else(|| current.title.clone());

    let mut act = activity::Activity::new()
        .activity_type(activity::ActivityType::Listening)
        .details(details_text);
    if let Some(state_text) = &state_text {
        act = act.state(state_text);
    }

    if !current.status_text.trim().is_empty() {
        let custom_name = render_status_text(
            &current.status_text,
            &current.title,
            &current.artist,
            &current.album,
        );
        if !custom_name.trim().is_empty() {
            act = act.name(custom_name);
        }
    }

    // Timestamps: show elapsed time while playing
    let timestamps;
    if current.is_playing && current.play_start_epoch > 0 {
        timestamps = if current.duration_secs > 0.0 {
            activity::Timestamps::new()
                .start(current.play_start_epoch)
                .end(current.play_start_epoch + current.duration_secs as i64)
        } else {
            activity::Timestamps::new().start(current.play_start_epoch)
        };
        act = act.timestamps(timestamps);
    }

    // Album art + quality hover text
    let assets;
    if !current.art_url.is_empty() {
        let mut a = activity::Assets::new()
            .large_image(current.art_url.clone())
            .small_image(SLATE_LOGO_URL)
            .small_text("Slate for TIDAL");
        if !current.quality_text.is_empty() {
            a = a.large_text(current.quality_text.clone());
        }
        assets = a;
        act = act.assets(assets);
    }

    // "Listen on TIDAL" button
    let buttons;
    if !current.url.is_empty() {
        buttons = vec![activity::Button::new(
            "Listen on TIDAL",
            current.url.clone(),
        )];
        act = act.buttons(buttons);
    }

    if let Err(e) = client.set_activity(act) {
        log::warn!("Failed to set Discord activity: {e}");
        return Err(());
    }
    Ok(())
}

fn render_status_text(template: &str, title: &str, artist: &str, album: &str) -> String {
    template
        .replace("{track}", title)
        .replace("{artist}", artist)
        .replace("{album}", album)
}
