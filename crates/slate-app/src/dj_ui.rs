//! The DJ page: connecting a chat model, the DJ's current block, its spoken line, a few
//! facts about the genre, the direction box and the live queue (the queue panel, shown
//! beside the page). Nothing here runs until the page opens or the DJ installs a block.

use std::sync::Mutex;

use slate_core::commands::utility;
use slate_core::controller::Outcome;
use slate_core::dj::{Dj, DjSettings, PRESETS};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::backend::Backend;
use crate::bootstrap::{AppWindow, DjState, SettingsState};
use crate::pages::Route;

/// The block the DJ installed last (from its `dj-branch` event). `None` until the DJ
/// has picked one this session.
#[derive(Clone, Default)]
struct Block {
    query: String,
    reason: String,
    line: String,
}

static LAST: Mutex<Option<Block>> = Mutex::new(None);

fn last() -> std::sync::MutexGuard<'static, Option<Block>> {
    LAST.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What the page shows for a block: heading for the facts, and the facts. A genre the
/// built-in list does not know gets a pointer to the direction box instead.
fn facts_for(block: &Block) -> (String, Vec<String>) {
    if block.query.is_empty() {
        return (String::new(), Vec::new());
    }
    match slate_core::dj_trivia::lookup(&block.query) {
        Some(g) => (
            format!("ABOUT {}", g.name.to_uppercase()),
            g.facts.iter().map(|f| (*f).to_string()).collect(),
        ),
        None => (
            "ABOUT THIS GENRE".into(),
            vec!["No facts for this one yet. Tell the DJ where to go next under Direction.".into()],
        ),
    }
}

/// How the lines are heard with the chosen voice: Piper says what is missing, if anything.
fn voice_text(dj: &DjSettings, program: Option<&str>, path: &std::ffi::OsStr) -> String {
    if slate_core::dj_voice::Voice::parse(&dj.voice) == slate_core::dj_voice::Voice::Piper {
        return match slate_core::dj_voice::piper_status(&dj.piper_model, path) {
            Ok(()) => "When the DJ has something to say, it is read aloud with Piper.".into(),
            Err(why) => why,
        };
    }
    speech_text(program)
}

/// Whether the DJ's spoken lines (the model decides when to speak) are heard here.
fn speech_text(program: Option<&str>) -> String {
    match program {
        Some(p) => format!("When the DJ has something to say, it is read aloud through {p}."),
        None => "TTS unavailable on this machine: install espeak-ng to hear the DJ's lines.".into(),
    }
}

/// What to say after a preset is picked.
fn preset_hint(name: &str, needs_key: bool) -> String {
    if needs_key {
        format!("{name}: paste your API key, then press Find models.")
    } else {
        format!("{name} runs on this computer; looking for its models…")
    }
}

/// The endpoint, key and model typed into the form.
fn form(w: &AppWindow) -> (String, String, String) {
    let g = w.global::<DjState>();
    (
        g.get_endpoint().trim().to_string(),
        g.get_api_key().trim().to_string(),
        g.get_model().trim().to_string(),
    )
}

fn set_models(g: &DjState<'_>, models: &[String]) {
    let rows: Vec<SharedString> = models.iter().map(Into::into).collect();
    g.set_models(ModelRc::new(VecModel::from(rows)));
    // One model, or none chosen yet: pick the first.
    if g.get_model().is_empty() || models.len() == 1 {
        if let Some(first) = models.first() {
            g.set_model(first.into());
        }
    }
}

impl Backend {
    /// The DJ installed a block (core event `dj-branch`).
    pub(crate) fn dj_branch(&self, payload: &serde_json::Value) {
        let text = |k: &str| {
            payload
                .get(k)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let block = Block {
            query: text("query"),
            reason: text("reason"),
            line: text("line"),
        };
        *last() = Some(block.clone());
        let status = self
            .controller()
            .and_then(|c| c.dj_status())
            .unwrap_or_default();
        self.ui(move |w| {
            // Only the page shows it; while another page is open this is a few strings.
            if w.get_view() == "dj" {
                show_block(&w.global::<DjState>(), &block, status);
            }
        });
    }

    /// Opens the DJ page. It turns the DJ on when it has an endpoint and a model, and
    /// shows the connection form when it has not.
    pub(crate) fn show_dj(&self) {
        self.ui(|w| w.set_view("dj".into()));
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let st = std::sync::Arc::clone(&state);
            let (dj, speech) = tokio::task::spawn_blocking(move || {
                let mut dj = utility::get_dj(&st);
                if slate_core::dj::is_configured(&dj) && !dj.enabled {
                    dj.enabled = true;
                    if let Err(e) = utility::set_dj(&st, dj.clone()) {
                        tracing::warn!("could not turn the DJ on: {}", e.log_safe());
                        dj.enabled = false;
                    }
                }
                let path = std::env::var_os("PATH").unwrap_or_default();
                let speech = voice_text(&dj, slate_core::dj::speech_program(), &path);
                (dj, speech)
            })
            .await
            .unwrap_or_default();
            let configured = slate_core::dj::is_configured(&dj);
            let block = last().clone().unwrap_or_default();
            let status = this
                .controller()
                .and_then(|c| c.dj_status())
                .unwrap_or_default();
            this.ui(move |w| {
                let g = w.global::<DjState>();
                g.set_configured(configured);
                g.set_on(dj.enabled);
                g.set_editing(false);
                g.set_busy(false);
                g.set_speech(speech.into());
                g.set_voice_index(i32::from(
                    slate_core::dj_voice::Voice::parse(&dj.voice)
                        == slate_core::dj_voice::Voice::Piper,
                ));
                g.set_piper_model(dj.piper_model.clone().into());
                g.set_duck(!dj.no_duck);
                g.set_endpoint(dj.base_url.into());
                g.set_api_key(dj.api_key.into());
                g.set_model(dj.model.into());
                g.set_connect_status("".into());
                show_block(&g, &block, status);
                // The Settings page reads this when it opens; keep it in step.
                w.global::<SettingsState>().set_dj_enabled(dj.enabled);
            });
        });
    }

    /// A button on the DJ page.
    pub(crate) fn dj_page_action(&self, action: &str) {
        if let Some(i) = action.strip_prefix("preset:") {
            self.dj_preset(i.parse().unwrap_or(usize::MAX));
            return;
        }
        if let Some(m) = action.strip_prefix("model:") {
            let m = m.to_string();
            self.ui(move |w| w.global::<DjState>().set_model(m.into()));
            return;
        }
        if let Some(v) = action.strip_prefix("voice:") {
            self.dj_voice_change(Some(v == "1"), None, None);
            return;
        }
        match action {
            "piper-model" => {
                let this = self.clone();
                self.rt.spawn(async move {
                    let m = this
                        .read_ui(|w| w.global::<DjState>().get_piper_model().to_string())
                        .await;
                    this.dj_voice_change(None, Some(m), None);
                });
            }
            "piper-browse" => self.dj_pick_voice_file(),
            "duck-on" => self.dj_voice_change(None, None, Some(true)),
            "duck-off" => self.dj_voice_change(None, None, Some(false)),
            "voice-test" => self.dj_voice_test(),
            "go" => self.dj_send_direction(),
            "now" => self.dj_now(),
            "settings" => {
                self.ui(|w| w.global::<SettingsState>().set_tab(2));
                self.navigate(Route::Settings);
            }
            "toggle" => self.dj_toggle(),
            "edit" => self.dj_edit(),
            "find-models" => self.dj_find_models(),
            "detect" => self.dj_detect(),
            "connect" => self.dj_connect(),
            _ => {}
        }
    }

    /// "Change endpoint": the form starts from what is saved, not from edits that a
    /// "Cancel" left behind.
    fn dj_edit(&self) {
        let Some(state) = self.state() else { return };
        let this = self.clone();
        self.rt.spawn(async move {
            let dj = tokio::task::spawn_blocking(move || utility::get_dj(&state))
                .await
                .unwrap_or_default();
            this.ui(move |w| {
                let g = w.global::<DjState>();
                g.set_endpoint(dj.base_url.into());
                g.set_api_key(dj.api_key.into());
                g.set_model(dj.model.into());
                g.set_models(ModelRc::default());
                g.set_connect_status("".into());
                g.set_editing(true);
            });
        });
    }

    /// Right-click a playlist → "DJ this playlist": the model orders its tracks into a set.
    pub(crate) fn dj_playlist(&self, uuid: &str) {
        let uuid = uuid.to_string();
        let this = self.clone();
        self.rt.spawn(async move {
            let (Some(state), Some(ctl)) = (this.state(), this.controller()) else {
                return;
            };
            let st = std::sync::Arc::clone(&state);
            let configured = tokio::task::spawn_blocking(move || {
                slate_core::dj::is_configured(&utility::get_dj(&st))
            })
            .await
            .unwrap_or(false);
            if !configured {
                this.notify("Set up the DJ first (DJ page): it needs a model endpoint.");
                return;
            }
            // The same disclosure as the DJ page, because this sends more.
            this.notify(
                "Asking the DJ: titles and artists of up to 200 tracks go to your DJ endpoint (a local server keeps them on this computer).",
            );
            let all = match crate::pages::fetch_all(
                &state,
                &this,
                &crate::library::SourceKind::Playlist(uuid.clone()),
            )
            .await
            {
                Ok(a) if !a.is_empty() => a,
                Ok(_) => return this.notify("That playlist is empty."),
                Err(e) => return this.notify(&format!("Could not load the playlist: {e}")),
            };
            let source = slate_core::queue::Source {
                kind: "playlist".into(),
                id: serde_json::Value::from(uuid),
                name: String::new(),
                image: None,
                subtitle: None,
                mix_type: None,
                tracks: slate_core::queue::source_tracks(&all),
            };
            let (outcome, shuffle_off, status) = ctl.dj_playlist(all, Some(source)).await;
            if shuffle_off {
                this.shared
                    .shuffle
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                this.ui(|w| w.set_shuffle(false));
            }
            this.apply(outcome).await;
            this.notify(&status);
        });
    }

    /// "Send": the DJ picks a block in that direction now; it replaces what is up next
    /// and plays when the current track ends.
    fn dj_send_direction(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let text = this
                .read_ui(|w| w.global::<DjState>().get_direction().to_string())
                .await;
            let text = text.trim().to_string();
            if text.is_empty() {
                return;
            }
            let Some(ctl) = this.controller() else { return };
            let asking = format!("Steering the DJ: “{text}”…");
            this.ui(move |w| {
                let g = w.global::<DjState>();
                g.set_direction("".into());
                g.set_status(asking.into());
            });
            let outcome = ctl.dj_steer(text).await;
            let picked = ctl.dj_status().unwrap_or_default();
            let queued = matches!(outcome, Outcome::Nothing) && picked.starts_with("DJ is up");
            this.apply(outcome).await;
            let status = if queued {
                format!("{picked} Up next after this track.")
            } else {
                picked
            };
            this.ui(move |w| w.global::<DjState>().set_status(status.into()));
        });
    }

    /// "DJ up a block": the status line says what happened either way.
    fn dj_now(&self) {
        self.ui(|w| {
            w.global::<DjState>()
                .set_status("Asking the DJ for a block…".into())
        });
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(ctl) = this.controller() else { return };
            let outcome = ctl.dj_next().await;
            let failed = matches!(outcome, Outcome::Nothing);
            this.apply(outcome).await;
            let status = ctl.dj_status().unwrap_or_else(|| {
                if failed {
                    "The DJ could not pick a block. Check its endpoint.".into()
                } else {
                    String::new()
                }
            });
            this.ui(move |w| w.global::<DjState>().set_status(status.into()));
        });
    }

    /// Saves a change to the voice settings and refreshes the line under the page.
    fn dj_voice_change(&self, piper: Option<bool>, model: Option<String>, duck: Option<bool>) {
        let Some(state) = self.state() else { return };
        let this = self.clone();
        self.rt.spawn(async move {
            let saved = tokio::task::spawn_blocking(move || {
                let mut dj = utility::get_dj(&state);
                if let Some(p) = piper {
                    dj.voice = if p { "piper".into() } else { "system".into() };
                }
                if let Some(m) = model {
                    dj.piper_model = m.trim().to_string();
                }
                if let Some(d) = duck {
                    dj.no_duck = !d;
                }
                utility::set_dj(&state, dj.clone())?;
                let path = std::env::var_os("PATH").unwrap_or_default();
                Ok::<_, slate_core::SlateError>(voice_text(
                    &dj,
                    slate_core::dj::speech_program(),
                    &path,
                ))
            })
            .await;
            match saved {
                Ok(Ok(text)) => this.ui(move |w| {
                    let g = w.global::<DjState>();
                    g.set_speech(text.into());
                    // The pick may have changed more than one control: re-read what is saved.
                }),
                Ok(Err(e)) => {
                    let msg = format!("Could not save the DJ voice: {}", e.log_safe());
                    this.ui(move |w| w.set_error(msg.into()));
                }
                Err(_) => {}
            }
        });
    }

    /// "Choose voice…": a file dialog for the Piper `.onnx` model.
    fn dj_pick_voice_file(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title("Choose a Piper voice")
                .add_filter("Piper voice", &["onnx"])
                .pick_file()
                .await
            else {
                return;
            };
            let path = file.path().to_string_lossy().into_owned();
            let shown = path.clone();
            this.ui(move |w| w.global::<DjState>().set_piper_model(shown.into()));
            this.dj_voice_change(None, Some(path), None);
        });
    }

    /// "Test voice": speaks a short line with the chosen voice, or says what is missing.
    fn dj_voice_test(&self) {
        let Some(state) = self.state() else { return };
        let this = self.clone();
        self.rt.spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                let dj = utility::get_dj(&state);
                let line = "Slate DJ here. This is how I sound.";
                if slate_core::dj_voice::Voice::parse(&dj.voice)
                    == slate_core::dj_voice::Voice::Piper
                {
                    slate_core::dj_voice::speak_piper(line, &dj.piper_model, None, None)
                } else {
                    slate_core::dj::speak(Some(line));
                    Ok(())
                }
            })
            .await;
            if let Ok(Err(why)) = result {
                this.ui(move |w| w.set_error(why.into()));
            }
        });
    }

    fn dj_toggle(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let on = tokio::task::spawn_blocking(move || {
                let mut dj = utility::get_dj(&state);
                dj.enabled = !dj.enabled;
                utility::set_dj(&state, dj.clone()).map(|()| dj.enabled)
            })
            .await;
            match on {
                Ok(Ok(on)) => this.ui(move |w| {
                    w.global::<DjState>().set_on(on);
                    w.global::<SettingsState>().set_dj_enabled(on);
                }),
                Ok(Err(e)) => {
                    let msg = format!("Could not save the DJ setting: {}", e.log_safe());
                    this.ui(move |w| w.set_error(msg.into()));
                }
                Err(_) => {}
            }
        });
    }

    fn dj_preset(&self, index: usize) {
        let Some(&(name, base, needs_key)) = PRESETS.get(index) else {
            return;
        };
        let hint = preset_hint(name, needs_key);
        self.ui(move |w| {
            let g = w.global::<DjState>();
            g.set_endpoint(base.into());
            g.set_models(ModelRc::default());
            g.set_model("".into());
            if !needs_key {
                g.set_api_key("".into());
            }
            g.set_connect_status(hint.into());
        });
        if !needs_key {
            self.dj_find_models();
        }
    }

    /// Asks the endpoint in the form for its models.
    fn dj_find_models(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let (endpoint, key, _) = this.read_ui(|w| form(&w)).await;
            if endpoint.is_empty() {
                return;
            }
            this.ui(|w| w.global::<DjState>().set_busy(true));
            let dj = Dj::new(
                DjSettings {
                    base_url: endpoint,
                    api_key: key,
                    ..DjSettings::default()
                },
                state.proxied_http.clone(),
            );
            let result = dj.models().await;
            this.ui(move |w| {
                let g = w.global::<DjState>();
                g.set_busy(false);
                match result {
                    Ok(models) if models.is_empty() => g.set_connect_status(
                        "The endpoint answered but lists no models. Type the model's name.".into(),
                    ),
                    Ok(models) => {
                        g.set_connect_status(format!("{} models. Pick one.", models.len()).into());
                        set_models(&g, &models);
                    }
                    Err(e) => g.set_connect_status(
                        format!("Could not list models: {e}. You can still type the model's name.")
                            .into(),
                    ),
                }
            });
        });
    }

    /// Looks for Ollama or LM Studio on this computer.
    fn dj_detect(&self) {
        let Some(state) = self.state() else { return };
        let this = self.clone();
        self.ui(|w| {
            let g = w.global::<DjState>();
            g.set_busy(true);
            g.set_connect_status("Looking for Ollama and LM Studio on this computer…".into());
        });
        self.rt.spawn(async move {
            let found = slate_core::dj::detect_local(&state.proxied_http).await;
            this.ui(move |w| {
                let g = w.global::<DjState>();
                g.set_busy(false);
                match found {
                    Some((name, base, models)) => {
                        g.set_endpoint(base.into());
                        g.set_api_key("".into());
                        g.set_model("".into());
                        set_models(&g, &models);
                        g.set_connect_status(
                            format!("Found {name} with {} models.", models.len()).into(),
                        );
                    }
                    None => g.set_connect_status(
                        "No local server answered (Ollama on port 11434, LM Studio on 1234). \
                         Start one, or pick a hosted endpoint."
                            .into(),
                    ),
                }
            });
        });
    }

    /// Tests the form's endpoint exactly as the DJ will use it, and saves it and turns
    /// the DJ on only when the model answers.
    fn dj_connect(&self) {
        let this = self.clone();
        self.rt.spawn(async move {
            let Some(state) = this.state() else { return };
            let (endpoint, key, model) = this.read_ui(|w| form(&w)).await;
            if endpoint.is_empty() || model.is_empty() {
                return;
            }
            this.ui(|w| {
                let g = w.global::<DjState>();
                g.set_busy(true);
                g.set_connect_status(
                    "Asking the model to answer (a local model may take a while to load)…".into(),
                );
            });
            let st = std::sync::Arc::clone(&state);
            let mut dj = tokio::task::spawn_blocking(move || utility::get_dj(&st))
                .await
                .unwrap_or_default();
            dj.base_url = endpoint;
            dj.api_key = key;
            dj.model = model;
            dj.enabled = true;
            let result = match utility::dj_test_endpoint(dj.clone(), &state).await {
                Ok(msg) => {
                    let st = std::sync::Arc::clone(&state);
                    let d = dj.clone();
                    tokio::task::spawn_blocking(move || utility::set_dj(&st, d))
                        .await
                        .map_err(|e| e.to_string())
                        .and_then(|r| r.map_err(|e| e.log_safe()))
                        .map(|()| msg)
                }
                Err(e) => Err(e),
            };
            this.ui(move |w| {
                let g = w.global::<DjState>();
                g.set_busy(false);
                match result {
                    Ok(msg) => {
                        g.set_configured(true);
                        g.set_on(true);
                        g.set_editing(false);
                        g.set_connect_status("".into());
                        g.set_status(format!("{msg}. The DJ is on.").into());
                        let s = w.global::<SettingsState>();
                        s.set_dj_enabled(true);
                        s.set_dj_base_url(dj.base_url.into());
                        s.set_dj_model(dj.model.into());
                        s.set_dj_api_key(dj.api_key.into());
                        s.set_dj_status("Connected".into());
                    }
                    Err(e) => g.set_connect_status(format!("Not connected: {e}").into()),
                }
            });
        });
    }
}

fn show_block(g: &DjState<'_>, block: &Block, status: String) {
    let (title, facts) = facts_for(block);
    g.set_genre(block.query.clone().into());
    g.set_reason(block.reason.clone().into());
    g.set_line(block.line.clone().into());
    g.set_facts_title(title.into());
    let facts: Vec<SharedString> = facts.into_iter().map(Into::into).collect();
    g.set_facts(ModelRc::new(VecModel::from(facts)));
    g.set_status(status.into());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(query: &str, reason: &str) -> Block {
        Block {
            query: query.into(),
            reason: reason.into(),
            line: String::new(),
        }
    }

    #[test]
    fn a_known_genre_shows_its_facts() {
        let (title, facts) = facts_for(&block("deep house", ""));
        assert_eq!(title, "ABOUT DEEP HOUSE");
        assert!(!facts.is_empty());
    }

    #[test]
    fn an_unknown_genre_points_to_the_direction_box() {
        let (title, facts) = facts_for(&block("polka from mars", "a change of pace"));
        assert_eq!(title, "ABOUT THIS GENRE");
        assert!(facts[0].contains("Direction"));
        assert!(
            facts_for(&block("", "")).1.is_empty(),
            "no block yet, no section"
        );
    }

    #[test]
    fn speech_says_what_happens() {
        assert!(speech_text(Some("espeak-ng")).contains("espeak-ng"));
        assert!(speech_text(None).starts_with("TTS unavailable"));
    }

    #[test]
    fn presets_say_what_to_do_next() {
        assert!(preset_hint("OpenAI", true).contains("API key"));
        assert!(preset_hint("Ollama", false).contains("this computer"));
        assert_eq!(
            PRESETS.iter().filter(|p| !p.2).count(),
            2,
            "two local presets"
        );
    }
}
