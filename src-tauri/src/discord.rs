use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

const APPLICATION_ID: &str = "1482171472167436308";

pub enum DiscordCommand {
    SetMetadata {
        title: String,
        artist: String,
        album: String,
        art_url: String,
        duration_secs: f64,
    },
    SetPlaying {
        is_playing: bool,
    },
    Stop,
    Connect,
    Disconnect,
}

pub struct DiscordHandle {
    tx: mpsc::Sender<DiscordCommand>,
}

impl DiscordHandle {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<DiscordCommand>();

        std::thread::spawn(move || {
            let mut client = DiscordIpcClient::new(APPLICATION_ID);

            let mut connected = false;
            let mut current_title = String::new();
            let mut current_artist = String::new();
            let mut current_album = String::new();
            let mut current_art_url = String::new();
            let mut current_duration_secs: f64 = 0.0;
            let mut is_playing = false;
            let mut play_start_epoch: i64 = 0;

            for cmd in rx {
                match cmd {
                    DiscordCommand::Connect => {
                        if !connected {
                            match client.connect() {
                                Ok(()) => {
                                    connected = true;
                                    log::info!("Discord Rich Presence connected");
                                }
                                Err(e) => {
                                    log::warn!("Failed to connect Discord IPC: {e}");
                                }
                            }
                        }
                    }
                    DiscordCommand::Disconnect => {
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
                    } => {
                        current_title = title;
                        current_artist = artist;
                        current_album = album;
                        current_art_url = art_url;
                        current_duration_secs = duration_secs;

                        if is_playing {
                            play_start_epoch = now_epoch_secs();
                        }

                        if connected {
                            set_activity(
                                &mut client,
                                &current_title,
                                &current_artist,
                                &current_album,
                                &current_art_url,
                                current_duration_secs,
                                is_playing,
                                play_start_epoch,
                            );
                        }
                    }
                    DiscordCommand::SetPlaying { is_playing: playing } => {
                        is_playing = playing;

                        if playing {
                            play_start_epoch = now_epoch_secs();
                        }

                        if connected {
                            if current_title.is_empty() && !playing {
                                client.clear_activity().ok();
                            } else {
                                set_activity(
                                    &mut client,
                                    &current_title,
                                    &current_artist,
                                    &current_album,
                                    &current_art_url,
                                    current_duration_secs,
                                    is_playing,
                                    play_start_epoch,
                                );
                            }
                        }
                    }
                    DiscordCommand::Stop => {
                        is_playing = false;
                        current_title.clear();
                        if connected {
                            client.clear_activity().ok();
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

fn now_epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn set_activity(
    client: &mut DiscordIpcClient,
    title: &str,
    artist: &str,
    album: &str,
    art_url: &str,
    duration_secs: f64,
    is_playing: bool,
    play_start_epoch: i64,
) {
    let state_text = if artist.is_empty() {
        album.to_string()
    } else if album.is_empty() {
        format!("by {artist}")
    } else {
        format!("by {artist}")
    };

    let mut act = activity::Activity::new()
        .activity_type(activity::ActivityType::Listening)
        .details(title)
        .state(&state_text);

    // Timestamps: show elapsed time while playing
    let timestamps;
    if is_playing && play_start_epoch > 0 {
        timestamps = if duration_secs > 0.0 {
            activity::Timestamps::new()
                .start(play_start_epoch)
                .end(play_start_epoch + duration_secs as i64)
        } else {
            activity::Timestamps::new().start(play_start_epoch)
        };
        act = act.timestamps(timestamps);
    }

    // Album art
    let assets;
    if !art_url.is_empty() {
        assets = activity::Assets::new()
            .large_image(art_url)
            .large_text(album);
        act = act.assets(assets);
    }

    if let Err(e) = client.set_activity(act) {
        log::warn!("Failed to set Discord activity: {e}");
        // Try to reconnect on next opportunity
    }
}
