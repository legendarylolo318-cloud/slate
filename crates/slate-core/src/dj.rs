//! DJ: LLM-driven continuity across genre blocks.
//!
//! When the queue runs dry the controller asks a local LLM (any
//! OpenAI-compatible `/chat/completions` endpoint, e.g. Ollama) to pick the next
//! genre, searches TIDAL for a block of tracks in that style, and queues them as
//! the next autoplay block. Any failure falls back to the existing TRACK_MIX
//! radio autoplay, so this feature can never wedge playback.
//!
//! Off by default: `Settings.dj.enabled` (or `DJ_ENABLE=1`) turns it on.

use serde::{Deserialize, Serialize};

use crate::sync::LockExt;

/// LLM DJ settings. All optional; `DJ_*` env vars override blanks at call time.
#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DjSettings {
    /// Master switch. Default: off.
    pub enabled: bool,
    /// OpenAI-compatible base URL, e.g. `http://127.0.0.1:11434/v1`.
    pub base_url: String,
    /// Bearer token. Blank = no Authorization header (local servers).
    pub api_key: String,
    /// Model name, e.g. `qwen3.8:latest`.
    pub model: String,
    /// Ask the LLM for a short spoken line for each block (display/TTS'd).
    pub speak: bool,
    /// Who reads the line: "" / "system" (espeak-ng or say) or "piper".
    pub voice: String,
    /// Path of the Piper voice (.onnx).
    pub piper_model: String,
    /// Leave the music at full volume while the line is spoken (ducked by default).
    pub no_duck: bool,
    /// Tracks per genre block at TIDAL search time. 0 = default (10).
    pub block_size: u32,
    /// Seconds to wait for the model's answer. 0 = default (300). Local models
    /// can be slow — loading an 8B VLM into VRAM plus generating can take well
    /// over a minute, so the default is generous (clamped to 1..=3600).
    pub timeout_secs: u64,
}

/// The API key never reaches a log line, whatever prints these settings.
impl std::fmt::Debug for DjSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DjSettings")
            .field("enabled", &self.enabled)
            .field("base_url", &self.base_url)
            .field(
                "api_key",
                &if self.api_key.is_empty() { "" } else { "<set>" },
            )
            .field("model", &self.model)
            .field("speak", &self.speak)
            .field("voice", &self.voice)
            .field("no_duck", &self.no_duck)
            .field("block_size", &self.block_size)
            .field("timeout_secs", &self.timeout_secs)
            .finish()
    }
}

impl DjSettings {
    /// Apply `DJ_ENABLE`, `DJ_BASE_URL`, `DJ_MODEL`, `DJ_API_KEY`, `DJ_SPEAK`,
    /// `DJ_BLOCK` env-var overrides on top of settings-file values.
    pub fn resolved(&self) -> Self {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let mut r = self.clone();
        if let Some(v) = env("DJ_ENABLE") {
            r.enabled = matches!(v.trim(), "1" | "true" | "yes" | "on");
        }
        if let Some(v) = env("DJ_BASE_URL") {
            r.base_url = v;
        }
        if let Some(v) = env("DJ_MODEL") {
            r.model = v;
        }
        if let Some(v) = env("DJ_API_KEY") {
            r.api_key = v;
        }
        if let Some(v) = env("DJ_SPEAK") {
            r.speak = matches!(v.trim(), "1" | "true" | "yes" | "on");
        }
        if let Some(v) = env("DJ_BLOCK").and_then(|v| v.trim().parse::<u32>().ok()) {
            r.block_size = v;
        }
        if let Some(v) = env("DJ_TIMEOUT").and_then(|v| v.trim().parse::<u64>().ok()) {
            r.timeout_secs = v;
        }
        r
    }
}

/// One DJ decision: the genre block to queue next.
#[derive(Debug, Clone, Serialize)]
pub struct DjMove {
    pub query: String,
    /// Artists for the block, the listener's own request first. TIDAL is searched by
    /// these (a style phrase alone matches song titles, not styles); at most 6.
    pub artists: Vec<String>,
    pub reason: String,
    pub speaks: bool,
    pub line: Option<String>,
}

/// The artist names in a reply: strings only, trimmed, at most 6 of at most 60 characters.
fn parse_artists(v: &serde_json::Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in v
        .get("artists")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
    {
        let a: String = a.trim().chars().take(60).collect();
        if !a.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(&a)) {
            out.push(a);
        }
        if out.len() == 6 {
            break;
        }
    }
    out
}

/// Parse the model's reply into a [`DjMove`]. Accepts plain JSON or JSON wrapped
/// in prose (outermost braces). `None` = unusable (caller falls back).
pub fn parse_move(raw: &str) -> Option<DjMove> {
    let text = raw.trim();
    let v = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(v) => v,
        Err(_) => {
            let start = text.find('{')?;
            let end = text.rfind('}')?;
            if end <= start {
                return None;
            }
            serde_json::from_str(&text[start..=end]).ok()?
        }
    };
    let query = v.get("query")?.as_str()?.trim().to_string();
    if query.is_empty() {
        return None;
    }
    Some(DjMove {
        query,
        artists: parse_artists(&v),
        reason: v
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string(),
        speaks: v
            .get("speaks")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        line: v
            .get("line")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    })
}

/// Snapshot of what is playing and what the listener asked for.
#[derive(Debug, Clone, Default)]
pub struct DjContext {
    /// (title, artist) of the current track, if any.
    pub current: Option<(String, String)>,
    /// Up to the last ~20 played tracks, oldest → newest, (title, artist).
    pub recent: Vec<(String, String)>,
    /// The listener's latest explicit direction, if any.
    pub direction: Option<String>,
    /// Why the current block was chosen (previous DJ reason).
    pub last_reason: Option<String>,
}

pub const SYSTEM_PROMPT: &str = "You are the DJ for a continuous music session. The listener asked you to keep the night going: when one genre block ends, choose the next genre or style so the set flows like one long DJ mix.
Rules:
- Keep the momentum: usually stay near the current genre; drift to adjacent styles, similar energy, or a smooth deliberate contrast.
- Vary the set: do not repeat the same query twice in a row and do not just ping-pong between two styles.
- If the listener gave a direction this turn, follow it; it outranks your own flow. When they name an artist (\"play King Von\"), that artist comes first in \"artists\" and the rest are close in sound.
- Respond with ONLY this JSON: {\"query\": \"<2-6 word genre/style phrase for the block>\", \"artists\": [\"<3 to 6 real artists who fit the block>\"], \"reason\": \"<short clause: why it follows>\", \"speaks\": <true|false>, \"line\": \"<one short sentence to say to the listener; only when speaks is true and it helps>\"}.
- \"query\" is a generic genre/style phrase (like \"deep house 124 bpm\" or \"late night soul\"). \"artists\" are real, well-known artists of that style; the music is found by searching for them.";

pub fn build_user_prompt(ctx: &DjContext) -> String {
    let mut out = String::new();
    if let Some((t, a)) = &ctx.current {
        out.push_str(&format!("current: {t} - {a}\n"));
    }
    if let Some(r) = &ctx.last_reason {
        out.push_str(&format!("why this block: {r}\n"));
    }
    if !ctx.recent.is_empty() {
        out.push_str("recent set (oldest first):\n");
        for (t, a) in &ctx.recent {
            out.push_str(&format!("  {t} - {a}\n"));
        }
    }
    if let Some(d) = &ctx.direction {
        out.push_str(&format!("listener direction (follow it): {d}\n"));
    }
    out.push_str("Pick the next block now.");
    out
}

/// Most tracks of a playlist sent to the model for "DJ this playlist".
pub const SET_MAX_TRACKS: usize = 200;

pub const SET_SYSTEM_PROMPT: &str = "You are a DJ ordering a playlist into one flowing set. You get numbered tracks (title - artist). Return an order that builds an energy curve (open easy, build, peak, ease off), moves smoothly between genres and avoids the same artist back to back.
Respond with ONLY this JSON: {\"order\": [<track numbers, every one exactly once, in the order to play them>]}.";

/// The numbered track list the model orders: `0. Title - Artist`, one per line, at most
/// [`SET_MAX_TRACKS`], each field cut so one odd title cannot swamp the prompt.
pub fn set_prompt(tracks: &[(String, String)]) -> String {
    let clip = |s: &str| -> String {
        s.chars()
            .filter(|c| !c.is_control())
            .take(80)
            .collect::<String>()
            .trim()
            .to_string()
    };
    let mut out = String::from("tracks:\n");
    for (i, (title, artist)) in tracks.iter().take(SET_MAX_TRACKS).enumerate() {
        out.push_str(&format!("{i}. {} - {}\n", clip(title), clip(artist)));
    }
    out.push_str("Order them now.");
    out
}

/// The track numbers in a model's answer: `{"order":[…]}` or a bare array, possibly inside
/// a code fence or other words. `None` when no list of numbers is found.
pub fn parse_order(raw: &str) -> Option<Vec<i64>> {
    let numbers = |v: &serde_json::Value| -> Option<Vec<i64>> {
        v.as_array()?
            .iter()
            .map(|n| {
                n.as_i64()
                    .or_else(|| n.as_str().and_then(|s| s.trim().parse().ok()))
            })
            .collect()
    };
    let try_slice = |s: &str| -> Option<Vec<i64>> {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        numbers(&v).or_else(|| numbers(v.get("order")?))
    };
    if let Some(o) = try_slice(raw.trim()) {
        return Some(o);
    }
    for (open, close) in [('{', '}'), ('[', ']')] {
        if let (Some(a), Some(b)) = (raw.find(open), raw.rfind(close)) {
            if a < b {
                if let Some(o) = try_slice(&raw[a..=b]) {
                    return Some(o);
                }
            }
        }
    }
    None
}

/// A usable order of `0..n` from what the model returned: indexes out of range or repeated
/// are dropped and the ones it left out are appended in their original order, so every
/// track plays exactly once whatever the model did.
pub fn finish_order(returned: &[i64], n: usize) -> Vec<usize> {
    let mut seen = vec![false; n];
    let mut out = Vec::with_capacity(n);
    for &i in returned {
        if let Ok(i) = usize::try_from(i) {
            if i < n && !seen[i] {
                seen[i] = true;
                out.push(i);
            }
        }
    }
    out.extend((0..n).filter(|i| !seen[*i]));
    out
}

/// A line from the model made safe to hand to a speech program: control characters
/// dropped, at most 200 characters, and never starting with `-` (so it cannot be read
/// as an option).
pub fn speakable(line: &str) -> Option<String> {
    let cleaned: String = line.chars().filter(|c| !c.is_control()).take(200).collect();
    let cleaned = cleaned.trim().trim_start_matches('-').trim().to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// The first of `names` that is a file in a directory of `path` (a PATH-style list).
fn find_in_path(names: &[&'static str], path: &std::ffi::OsStr) -> Option<&'static str> {
    names.iter().copied().find(|n| {
        std::env::split_paths(path)
            .any(|d| d.join(n).is_file() || d.join(format!("{n}.exe")).is_file())
    })
}

/// Best-effort spoken line via a system TTS if one is on PATH. Never blocks the
/// calling thread for long, never fails playback. Purely cosmetic.
pub fn speak(line: Option<&str>) {
    let Some(line) = line.and_then(speakable) else {
        return;
    };
    let Some(bin) = speech_program() else {
        log::info!("[dj] would say: {line}");
        return;
    };
    std::thread::spawn(move || {
        if bin == "say" {
            let _ = std::process::Command::new(bin).arg(&line).output();
        } else {
            // espeak-ng has no `--text`; the line goes in on stdin, never as an argument.
            use std::io::Write;
            let child = std::process::Command::new(bin)
                .arg("--stdin")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
            if let Ok(mut child) = child {
                if let Some(mut input) = child.stdin.take() {
                    let _ = input.write_all(line.as_bytes());
                }
                let _ = child.wait();
            }
        }
    });
}

/// Whether the DJ has what it needs to ask a model: an endpoint and a model name
/// (from the settings or the `DJ_*` variables). The API key is optional.
pub fn is_configured(settings: &DjSettings) -> bool {
    let r = settings.resolved();
    !r.base_url.trim().is_empty() && !r.model.trim().is_empty()
}

/// The speech program `speak` would use (`espeak-ng` or `say`), if one is on PATH.
pub fn speech_program() -> Option<&'static str> {
    std::env::var_os("PATH").and_then(|p| find_in_path(&["espeak-ng", "say"], &p))
}

/// Endpoints the DJ page offers in one click: name, base URL, whether it needs a key.
pub const PRESETS: [(&str, &str, bool); 4] = [
    ("Ollama", "http://127.0.0.1:11434/v1", false),
    ("LM Studio", "http://127.0.0.1:1234/v1", false),
    ("OpenAI", "https://api.openai.com/v1", true),
    ("OpenRouter", "https://openrouter.ai/api/v1", true),
];

/// Model ids from an OpenAI-compatible `/models` answer (`{"data":[{"id":…}]}`).
pub fn parse_models(text: &str) -> Result<Vec<String>, String> {
    let v: serde_json::Value = serde_json::from_str(text)
        .map_err(|_| "The endpoint's model list was not JSON.".to_string())?;
    let mut ids: Vec<String> = v
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "The endpoint did not return a model list.".to_string())?
        .iter()
        .filter_map(|m| m.get("id").and_then(serde_json::Value::as_str))
        .map(|id| id.chars().take(120).collect())
        .collect();
    ids.sort();
    ids.dedup();
    ids.truncate(200);
    Ok(ids)
}

/// Looks for a chat server on this computer (the local presets), each given a short
/// timeout. Returns the first that lists models: its preset name, base URL and models.
pub async fn detect_local(
    http: &crate::proxy_http::ProxiedHttp,
) -> Option<(&'static str, &'static str, Vec<String>)> {
    for (name, base, needs_key) in PRESETS {
        if needs_key {
            continue;
        }
        let dj = Dj::new(
            DjSettings {
                base_url: base.to_string(),
                ..DjSettings::default()
            },
            http.clone(),
        );
        let found = tokio::time::timeout(std::time::Duration::from_millis(1500), dj.models()).await;
        if let Ok(Ok(models)) = found {
            if !models.is_empty() {
                return Some((name, base, models));
            }
        }
    }
    None
}

/// Client for one OpenAI-compatible chat endpoint. Never builds its own
/// connection: the app hands over its shared proxy-aware `ProxiedHttp`, so a
/// blocked proxy degrades the DJ to the radio fallback instead of egressing
/// direct (the rule `tests/source_guards.rs` enforces).
pub struct Dj {
    http: crate::proxy_http::ProxiedHttp,
    cfg: DjSettings,
    /// One line a human can read, explaining the most recent `decide`/`ping`
    /// outcome — success or the reason it could not move. Written by the DJ,
    /// read by the controller when `decide` returns `None`.
    last_diag: std::sync::Mutex<Option<String>>,
    /// The raw model text of the most recent answer (if any), so "nothing
    /// happened" can show *what the model actually said* instead of a guess.
    last_raw: std::sync::Mutex<Option<String>>,
}

impl Dj {
    pub fn new(settings: DjSettings, http: crate::proxy_http::ProxiedHttp) -> Self {
        Self {
            cfg: settings.resolved(),
            http,
            last_diag: std::sync::Mutex::new(None),
            last_raw: std::sync::Mutex::new(None),
        }
    }

    fn note_diag(&self, msg: impl Into<String>) {
        *self.last_diag.lock_safe() = Some(msg.into());
    }

    fn note_raw(&self, raw: &str) {
        *self.last_raw.lock_safe() = Some(raw.to_string());
    }

    /// Human-readable explanation of the most recent `decide`/`ping` outcome.
    pub fn last_diag(&self) -> Option<String> {
        self.last_diag.lock_safe().clone()
    }

    /// Raw model text of the most recent answer, if the endpoint answered at all.
    pub fn last_raw(&self) -> Option<String> {
        self.last_raw.lock_safe().clone()
    }

    pub fn enabled(&self) -> bool {
        self.cfg.enabled
    }

    pub fn block_size(&self) -> u32 {
        if self.cfg.block_size == 0 {
            10
        } else {
            self.cfg.block_size
        }
    }

    /// Effective per-request timeout in seconds: the configured value clamped
    /// to a sane range, 300 s when unset (0). Local models are slow — loading
    /// plus generating far outlasts a typical 45 s request cap, so the default
    /// waits long enough for a cold 8B model before giving up.
    pub fn timeout_secs(&self) -> u64 {
        match self.cfg.timeout_secs {
            0 => 300,
            s => s.clamp(1, 3600),
        }
    }

    /// Turn a transport-level failure (no answer at all) into a sentence that
    /// says what happened and what to try, instead of the bare reqwest text
    /// ("error sending request for url (…)") which reads like a bug in Slate.
    fn send_error(&self, e: reqwest::Error) -> String {
        let base = e.to_string();
        if e.is_connect() {
            return format!(
                "Could not reach the server — is it running at that address and port? ({base})"
            );
        }
        if e.is_timeout() {
            return format!(
                "The endpoint did not answer within {} s — a local model usually just needs more time while it loads or generates slowly. Wait a moment and test again, or raise the DJ timeout in Settings. ({base})",
                self.timeout_secs()
            );
        }
        base
    }

    /// Ask the LLM for the next block. `None` = unavailable or unparseable; the
    /// caller falls back to the usual autoplay radio. Every `None` leaves a
    /// human-readable reason in `last_diag()` (and the raw reply, if any, in
    /// `last_raw()`), so the UI can say *why* nothing moved instead of guessing.
    pub async fn decide(&self, ctx: &DjContext) -> Option<DjMove> {
        let base_url = self.cfg.base_url.trim();
        if base_url.is_empty() || self.cfg.model.trim().is_empty() {
            self.note_diag("The DJ endpoint or model is not filled in (Settings → Playback).");
            return None;
        }
        let url = format!(
            "{base_url}/chat/completions",
            base_url = base_url.trim_end_matches('/')
        );
        let user = build_user_prompt(ctx);
        let base = serde_json::json!({
            "model": self.cfg.model,
            "messages": [
                { "role": "system", "content": SYSTEM_PROMPT },
                { "role": "user", "content": user },
            ],
            "temperature": 0.9,
        });

        // 1) with an OpenAI-style JSON schema (Ollama accepts this too).
        let mut body = base.clone();
        body["response_format"] = serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": "dj_move",
                "strict": false,
                "schema": {
                    "type": "object",
                    "properties": {
                        "query":  { "type": "string" },
                        "artists": { "type": "array", "items": { "type": "string" } },
                        "reason": { "type": "string" },
                        "speaks": { "type": "boolean" },
                        "line":   { "type": "string" },
                    },
                    "required": ["query"],
                },
            },
        });
        match self.post_inner(&url, &body).await {
            Ok(raw) => {
                self.note_raw(&raw);
                if let Some(m) = parse_move(&raw) {
                    return Some(m);
                }
                self.note_diag(format!(
                    "The endpoint answered, but the reply was not a usable move: {}",
                    raw.chars().take(200).collect::<String>()
                ));
                return None;
            }
            // No answer at all: asking again without the schema would only wait out
            // the timeout a second time (the setting is the wait for the model).
            Err(PostError::Unreached(e)) => {
                self.note_diag(&e);
                return None;
            }
            Err(PostError::Refused(_)) => {}
        }
        // 2) retry plain, then parse whatever came back.
        match self.post(&url, &base).await {
            Ok(raw) => {
                self.note_raw(&raw);
                parse_move(&raw).or_else(|| {
                    self.note_diag(format!(
                        "The endpoint answered, but the reply was not a usable move: {}",
                        raw.chars().take(200).collect::<String>()
                    ));
                    None
                })
            }
            Err(e) => {
                // Keep the music flowing: the endpoint said no (or was slow),
                // the diagnosis for the UI is why, not silence.
                self.note_diag(&e);
                None
            }
        }
    }

    /// Asks the model to order `tracks` (title, artist; at most [`SET_MAX_TRACKS`] are sent)
    /// into a set. The answer is validated by [`finish_order`]; `Err` says why there was none
    /// (the caller then keeps the original order).
    pub async fn order_set(&self, tracks: &[(String, String)]) -> Result<Vec<usize>, String> {
        let base_url = self.cfg.base_url.trim();
        if base_url.is_empty() || self.cfg.model.trim().is_empty() {
            return Err("The DJ endpoint or model is not filled in.".into());
        }
        let n = tracks.len().min(SET_MAX_TRACKS);
        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": self.cfg.model,
            "messages": [
                { "role": "system", "content": SET_SYSTEM_PROMPT },
                { "role": "user", "content": set_prompt(&tracks[..n]) },
            ],
            "temperature": 0.4,
        });
        let raw = self.post(&url, &body).await?;
        self.note_raw(&raw);
        let order = parse_order(&raw).ok_or_else(|| {
            format!(
                "The reply was not a list of track numbers: {}",
                raw.chars().take(120).collect::<String>()
            )
        })?;
        Ok(finish_order(&order, n))
    }

    /// Connectivity check for the settings "Test endpoint" button: one minimal
    /// completion through exactly the same path the live DJ uses (saved
    /// settings, same client, same response handling). The `Ok` message is
    /// human-readable and includes latency; the `Err` message says what failed.
    pub async fn ping(&self) -> Result<String, String> {
        let base_url = self.cfg.base_url.trim();
        if base_url.is_empty() || self.cfg.model.trim().is_empty() {
            return Err(
                "Fill in the DJ endpoint and model first (Settings → Playback).".to_string(),
            );
        }
        let url = format!(
            "{base_url}/chat/completions",
            base_url = base_url.trim_end_matches('/')
        );
        let body = serde_json::json!({
            "model": self.cfg.model,
            "messages": [{ "role": "user", "content": "Reply with the single word: pong" }],
        });
        let started = std::time::Instant::now();
        let raw = self.post(&url, &body).await?;
        let ms = started.elapsed().as_millis();
        let head = raw.trim().chars().take(80).collect::<String>();
        Ok(format!(
            "Connected — '{}' answered in {ms} ms: {head}",
            self.cfg.model.trim()
        ))
    }

    /// The models the endpoint offers (`GET {base}/models`, the OpenAI-compatible list
    /// that Ollama, LM Studio, OpenAI and OpenRouter all serve), sorted, at most 200.
    pub async fn models(&self) -> Result<Vec<String>, String> {
        let base_url = self.cfg.base_url.trim().trim_end_matches('/');
        if base_url.is_empty() {
            return Err("Fill in the endpoint first.".into());
        }
        let client = self.http.client().map_err(|e| e.cause)?;
        let mut req = client
            .get(format!("{base_url}/models"))
            .timeout(std::time::Duration::from_secs(15));
        if !self.cfg.api_key.trim().is_empty() {
            req = req.bearer_auth(self.cfg.api_key.trim());
        }
        let resp = req.send().await.map_err(|e| self.send_error(e))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!(
                "HTTP {}: {}",
                status.as_u16(),
                text.chars().take(200).collect::<String>()
            ));
        }
        parse_models(&text)
    }

    async fn post(&self, url: &str, body: &serde_json::Value) -> Result<String, String> {
        self.post_inner(url, body).await.map_err(|e| match e {
            PostError::Unreached(e) | PostError::Refused(e) => e,
        })
    }

    async fn post_inner(&self, url: &str, body: &serde_json::Value) -> Result<String, PostError> {
        let client = self
            .http
            .client()
            .map_err(|e| PostError::Unreached(e.cause))?;
        let mut req = client
            .post(url)
            .json(body)
            // A hung endpoint must not wedge autoplay: the radio still plays —
            // but a local model may need minutes to load and generate, so the
            // cap is generous (settings → DJ timeout) rather than 45 s.
            .timeout(std::time::Duration::from_secs(self.timeout_secs()));
        if !self.cfg.api_key.trim().is_empty() {
            req = req.bearer_auth(self.cfg.api_key.trim());
        }
        let resp = req.send().await.map_err(|e| {
            let unreached = e.is_timeout() || e.is_connect();
            let msg = self.send_error(e);
            if unreached {
                PostError::Unreached(msg)
            } else {
                PostError::Refused(msg)
            }
        })?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| PostError::Refused(e.to_string()))?;
        if !status.is_success() {
            // By characters: the body is the endpoint's, and a byte cut can split one.
            return Err(PostError::Refused(format!(
                "HTTP {}: {}",
                status.as_u16(),
                text.chars().take(200).collect::<String>()
            )));
        }
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| PostError::Refused(e.to_string()))?;
        let content = v
            .pointer("/choices/0/message/content")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                PostError::Refused("no choices[0].message.content in response".to_string())
            })?;
        Ok(content.to_string())
    }
}

/// Why a request to the endpoint failed.
enum PostError {
    /// Nothing answered (connect failure, timeout, proxy blocked).
    Unreached(String),
    /// It answered, but not with a usable completion.
    Refused(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artists_are_read_trimmed_deduplicated_and_bounded() {
        let m = parse_move(
            r#"{"query":"chicago drill","artists":[" King Von ","Lil Durk","king von",7,"","A","B","C","D","E"],"reason":"asked"}"#,
        )
        .expect("move");
        assert_eq!(m.artists, vec!["King Von", "Lil Durk", "A", "B", "C", "D"]);
        let old =
            parse_move(r#"{"query":"deep house"}"#).expect("a reply without artists still works");
        assert!(old.artists.is_empty());
    }

    #[test]
    fn model_lists_parse_sorted_and_bounded() {
        let ids =
            parse_models(r#"{"object":"list","data":[{"id":"b"},{"id":"a"},{"id":"a"},{"x":1}]}"#)
                .expect("list");
        assert_eq!(ids, vec!["a", "b"]);
        assert!(parse_models("not json").is_err());
        assert!(parse_models(r#"{"models":[]}"#).is_err());
        let many = format!(
            r#"{{"data":[{}]}}"#,
            (0..300)
                .map(|i| format!(r#"{{"id":"m{i:03}"}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(parse_models(&many).expect("list").len(), 200);
    }

    #[tokio::test]
    async fn models_come_from_the_endpoint_list() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/v1/models"))
            .and(wiremock::matchers::header("authorization", "Bearer k"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_string(r#"{"data":[{"id":"gemma4:e4b"},{"id":"llama3"}]}"#),
            )
            .mount(&server)
            .await;
        let dj = Dj::new(
            DjSettings {
                base_url: format!("{}/v1/", server.uri()),
                api_key: "k".into(),
                ..DjSettings::default()
            },
            direct_http(),
        );
        assert_eq!(
            dj.models().await.expect("models"),
            vec!["gemma4:e4b", "llama3"]
        );
    }

    #[test]
    fn spoken_lines_are_cleaned_and_cannot_be_options() {
        assert_eq!(
            speakable("  hello there \n").as_deref(),
            Some("hello there")
        );
        assert_eq!(speakable("--help").as_deref(), Some("help"));
        assert_eq!(speakable("\u{7}\u{1b}[31m").as_deref(), Some("[31m"));
        assert_eq!(speakable("   "), None);
        assert_eq!(speakable(&"x".repeat(500)).map(|l| l.len()), Some(200));
    }

    #[test]
    fn the_speech_program_is_found_on_a_path_list() {
        let d = tempfile::tempdir().expect("tempdir");
        let other = tempfile::tempdir().expect("tempdir");
        std::fs::write(d.path().join("espeak-ng"), b"").expect("file");
        let path = std::env::join_paths([other.path(), d.path()]).expect("join");
        assert_eq!(
            find_in_path(&["espeak-ng", "say"], &path),
            Some("espeak-ng")
        );
        let empty = std::env::join_paths([other.path()]).expect("join");
        assert_eq!(find_in_path(&["espeak-ng", "say"], &empty), None);
    }

    #[test]
    fn parse_plain_json() {
        let m =
            parse_move(r#"{"query":"deep house","reason":"keeps the groove","speaks":true,"line":"Deep from here."}"#).unwrap();
        assert_eq!(m.query, "deep house");
        assert_eq!(m.reason, "keeps the groove");
        assert!(m.speaks);
        assert_eq!(m.line.as_deref(), Some("Deep from here."));
    }

    #[test]
    fn parse_json_wrapped_in_prose() {
        let m = parse_move("Sure thing!\n{\"query\":\"jazz fusion\",\"reason\":\"drift\"}\nEnjoy.")
            .unwrap();
        assert_eq!(m.query, "jazz fusion");
        assert!(!m.speaks);
        assert!(m.line.is_none());
    }

    #[test]
    fn parse_rejects_empty_query_and_garbage() {
        assert!(parse_move(r#"{"query":"   "}"#).is_none());
        assert!(parse_move("no json here at all").is_none());
        assert!(parse_move(r#"{"reason":"no query field"}"#).is_none());
        assert!(parse_move("").is_none());
    }

    #[test]
    fn prompt_covers_direction_recent_and_current() {
        let p = build_user_prompt(&DjContext {
            current: Some(("Song".into(), "Artist".into())),
            recent: vec![("A".into(), "B".into()), ("C".into(), "D".into())],
            direction: Some("more jazz".into()),
            last_reason: Some("flow".into()),
        });
        assert!(p.contains("Song - Artist"));
        assert!(p.contains("A - B"));
        assert!(p.contains("C - D"));
        assert!(p.contains("more jazz"));
        assert!(p.contains("flow"));
        assert!(p.contains("Pick the next block now."));
    }

    #[test]
    fn default_settings_are_off() {
        let s = DjSettings::default();
        assert!(!s.enabled);
        assert!(s.base_url.is_empty());
        assert_eq!(
            Dj::new(s, crate::proxy_http::ProxiedHttp::blocked("test")).block_size(),
            10
        );
    }

    /// The 45 s cap behind the user's "error sending request" timeout: a local
    /// model loading plus generating far outlasts it, so the effective timeout
    /// defaults generous and clamps to a sane range.
    #[test]
    fn effective_timeout_is_generous_and_clamped() {
        let mk = |secs: u64| {
            Dj::new(
                DjSettings {
                    timeout_secs: secs,
                    ..DjSettings::default()
                },
                crate::proxy_http::ProxiedHttp::blocked("test"),
            )
        };
        assert_eq!(
            mk(0).timeout_secs(),
            300,
            "unset (0) must mean the generous default"
        );
        assert_eq!(mk(60).timeout_secs(), 60);
        assert_eq!(
            mk(999_999).timeout_secs(),
            3600,
            "unusable caps must clamp down"
        );
    }

    /// A model that takes longer than the cap must read as "slow", not as an
    /// unexplained transport failure.
    #[tokio::test]
    async fn ping_names_the_timeout_when_the_model_is_slow() {
        use std::time::Duration;
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(chat_response("pong").set_delay(Duration::from_millis(2000)))
            .expect(1)
            .mount(&server)
            .await;
        let dj = Dj::new(
            DjSettings {
                base_url: server.uri(),
                model: "test-model".into(),
                timeout_secs: 1,
                ..Default::default()
            },
            direct_http(),
        );
        let err = dj.ping().await.expect_err("the 1 s cap must expire first");
        assert!(err.contains("within 1 s"), "got: {err}");
        assert!(err.contains("loads or generates slowly"), "got: {err}");
    }

    /// The other reading of the same reqwest text: the server was never there.
    #[tokio::test]
    async fn ping_names_an_unreachable_server() {
        // Port 1 on localhost: connection refused, deterministically, no DNS.
        let dj = Dj::new(
            DjSettings {
                base_url: "http://127.0.0.1:1/v1".into(),
                model: "test-model".into(),
                ..Default::default()
            },
            direct_http(),
        );
        let err = dj.ping().await.expect_err("nothing listens there");
        assert!(err.contains("Could not reach the server"), "got: {err}");
    }

    /// Opt-in live check against a real endpoint — the suite stays hermetic
    /// unless someone sets DJ_LIVE=1 plus DJ_BASE_URL/DJ_MODEL:
    ///   DJ_LIVE=1 DJ_BASE_URL=http://host:11434/v1 DJ_MODEL=qwen3-vl:8b \
    ///   DJ_API_KEY=*** cargo test -p slate-core ping_live \
    ///       -- --nocapture --ignored
    #[tokio::test]
    #[ignore]
    async fn ping_live_opt_in() {
        let base_url = std::env::var("DJ_BASE_URL").expect("DJ_BASE_URL");
        let model = std::env::var("DJ_MODEL").expect("DJ_MODEL");
        if !base_url.starts_with("http") || model.is_empty() {
            return;
        }
        let dj = Dj::new(
            DjSettings {
                base_url,
                model,
                api_key: std::env::var("DJ_API_KEY").unwrap_or_default(),
                ..DjSettings::default()
            },
            direct_http(),
        );
        let started = std::time::Instant::now();
        let msg = dj.ping().await.expect("the live endpoint must answer");
        let secs = started.elapsed().as_secs_f64();
        eprintln!("LIVE OK in {secs:.1} s: {msg}");
    }

    fn direct_http() -> crate::proxy_http::ProxiedHttp {
        crate::proxy_http::ProxiedHttp::from_settings(
            &crate::ProxySettings::default(),
            &crate::proxy::HostCaps::assume_all_present(),
        )
    }

    fn chat_response(content: &str) -> wiremock::ResponseTemplate {
        wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{ "message": { "content": content } }]
        }))
    }

    #[tokio::test]
    async fn ping_reports_a_successful_completion_with_latency() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(chat_response("pong"))
            .expect(1)
            .mount(&server)
            .await;
        let dj = Dj::new(
            DjSettings {
                enabled: true,
                base_url: server.uri(),
                model: "test-model".into(),
                ..Default::default()
            },
            direct_http(),
        );
        let msg = dj.ping().await.expect("ping should succeed");
        assert!(msg.contains("Connected"), "got: {msg}");
        assert!(msg.contains("pong"), "got: {msg}");
        assert!(msg.contains("ms"), "got: {msg}");
    }

    #[tokio::test]
    async fn ping_reports_an_http_error_instead_of_silence() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(401).set_body_string("invalid api key"))
            .expect(1)
            .mount(&server)
            .await;
        let dj = Dj::new(
            DjSettings {
                base_url: server.uri(),
                model: "test-model".into(),
                api_key: "sk-nope".into(),
                ..Default::default()
            },
            direct_http(),
        );
        let err = dj.ping().await.expect_err("a 401 must be an error");
        assert!(err.contains("401"), "got: {err}");
        assert!(err.contains("invalid api key"), "got: {err}");
    }

    #[tokio::test]
    async fn ping_refuses_while_the_proxy_is_blocked() {
        let dj = Dj::new(
            DjSettings {
                base_url: "https://example.invalid/v1".into(),
                model: "test-model".into(),
                ..Default::default()
            },
            crate::proxy_http::ProxiedHttp::blocked("test-block"),
        );
        let err = dj.ping().await.expect_err("blocked client must not egress");
        assert!(err.contains("test-block"), "got: {err}");
    }

    #[tokio::test]
    async fn ping_refuses_when_endpoints_are_not_filled_in() {
        let dj = Dj::new(
            DjSettings::default(),
            crate::proxy_http::ProxiedHttp::blocked("test-block"),
        );
        let err = dj
            .ping()
            .await
            .expect_err("settings missing must not touch the network");
        assert!(err.contains("endpoint"), "got: {err}");
    }

    #[tokio::test]
    async fn decide_which_fails_leaves_a_readable_diagnosis() {
        // Endpoint answers 200, but with prose, not the JSON the DJ needs.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(chat_response(
                "I think something slower and deeper works now.",
            ))
            .expect(1)
            .mount(&server)
            .await;
        let dj = Dj::new(
            DjSettings {
                base_url: server.uri(),
                model: "test-model".into(),
                ..Default::default()
            },
            direct_http(),
        );
        assert!(dj.decide(&DjContext::default()).await.is_none());
        let raw = dj.last_raw().expect("raw reply must be kept for the UI");
        assert!(raw.contains("slower and deeper"), "raw: {raw}");
        let diag = dj.last_diag().expect("a diagnosis must be left behind");
        assert!(diag.contains("not a usable move"), "diag: {diag}");
    }

    /// A first request that times out is not sent again without the schema: that would
    /// wait the whole timeout twice before the radio takes over.
    #[tokio::test]
    async fn decide_waits_out_a_silent_endpoint_once() {
        use std::time::Duration;
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(chat_response("{}").set_delay(Duration::from_millis(2500)))
            .expect(1)
            .mount(&server)
            .await;
        let dj = Dj::new(
            DjSettings {
                base_url: server.uri(),
                model: "test-model".into(),
                timeout_secs: 1,
                ..Default::default()
            },
            direct_http(),
        );
        assert!(dj.decide(&DjContext::default()).await.is_none());
        let diag = dj.last_diag().expect("diagnosis");
        assert!(diag.contains("within 1 s"), "diag: {diag}");
    }

    /// An endpoint that refuses the schema (HTTP 400) is still asked again plainly.
    #[tokio::test]
    async fn decide_retries_without_the_schema_when_it_is_refused() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::body_string_contains("response_format"))
            .respond_with(wiremock::ResponseTemplate::new(400).set_body_string("no schema"))
            .expect(1)
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(chat_response(r#"{"query":"dub","reason":"x"}"#))
            .expect(1)
            .mount(&server)
            .await;
        let dj = Dj::new(
            DjSettings {
                base_url: server.uri(),
                model: "test-model".into(),
                ..Default::default()
            },
            direct_http(),
        );
        let m = dj.decide(&DjContext::default()).await.expect("plain retry");
        assert_eq!(m.query, "dub");
    }

    /// The error body is the endpoint's: cutting it must not split a character (a panic).
    #[tokio::test]
    async fn an_error_body_with_wide_characters_is_cut_safely() {
        let server = wiremock::MockServer::start().await;
        let body = format!("{}é€ and more", "a".repeat(199));
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(500).set_body_string(body))
            .mount(&server)
            .await;
        let dj = Dj::new(
            DjSettings {
                base_url: server.uri(),
                model: "test-model".into(),
                ..Default::default()
            },
            direct_http(),
        );
        let err = dj.ping().await.expect_err("HTTP 500");
        assert!(err.starts_with("HTTP 500: ") && err.ends_with('é'), "{err}");
    }

    #[test]
    fn the_api_key_is_never_printed() {
        let s = DjSettings {
            api_key: "sk-secret-123".into(),
            ..Default::default()
        };
        let out = format!("{s:?} {:?}", s.resolved());
        assert!(!out.contains("sk-secret"), "{out}");
        assert!(out.contains("<set>"), "{out}");
    }

    #[tokio::test]
    async fn decide_which_succeeds_returns_the_move() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(chat_response(r#"{"query":"deep house","reason":"groove"}"#))
            .expect(1)
            .mount(&server)
            .await;
        let dj = Dj::new(
            DjSettings {
                base_url: server.uri(),
                model: "test-model".into(),
                ..Default::default()
            },
            direct_http(),
        );
        let m = dj
            .decide(&DjContext::default())
            .await
            .expect("a good reply must parse");
        assert_eq!(m.query, "deep house");
        assert_eq!(m.reason, "groove");
    }

    #[test]
    fn the_set_prompt_numbers_and_cuts_the_tracks() {
        let tracks: Vec<(String, String)> = (0..300)
            .map(|i| (format!("T{i}\nx"), "A".repeat(200)))
            .collect();
        let p = set_prompt(&tracks);
        assert!(p.contains("\n0. T0x - AAA") && p.contains("\n199. "));
        assert!(!p.contains("\n200. "), "at most 200 tracks");
        assert!(p.lines().all(|l| l.len() < 200));
    }

    #[test]
    fn orders_are_read_from_whatever_the_model_wrote() {
        assert_eq!(parse_order(r#"{"order":[2,0,1]}"#), Some(vec![2, 0, 1]));
        assert_eq!(parse_order("[1,0]"), Some(vec![1, 0]));
        assert_eq!(parse_order(r#"{"order":["1"," 0"]}"#), Some(vec![1, 0]));
        assert_eq!(
            parse_order("Sure!\n```json\n{\"order\": [3, 1, 2]}\n```"),
            Some(vec![3, 1, 2])
        );
        assert_eq!(parse_order("no idea"), None);
        assert_eq!(parse_order(r#"{"order":"3,1"}"#), None);
    }

    #[test]
    fn a_returned_order_is_made_valid() {
        // Duplicates and out-of-range numbers go; missing ones are appended in order.
        assert_eq!(finish_order(&[2, 2, 9, -1, 0], 5), vec![2, 0, 1, 3, 4]);
        assert_eq!(finish_order(&[], 3), vec![0, 1, 2]);
        assert_eq!(finish_order(&[1, 0], 2), vec![1, 0]);
        assert_eq!(finish_order(&[0], 0), Vec::<usize>::new());
        let o = finish_order(&[4, 4, 4, 100, 3], 6);
        let mut sorted = o.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..6).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn order_set_returns_a_valid_order_from_a_messy_reply() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(chat_response(r#"{"order":[2,2,7,0]}"#))
            .expect(1)
            .mount(&server)
            .await;
        let dj = Dj::new(
            DjSettings {
                base_url: server.uri(),
                model: "m".into(),
                ..Default::default()
            },
            direct_http(),
        );
        let tracks: Vec<(String, String)> = (0..4).map(|i| (format!("t{i}"), "a".into())).collect();
        assert_eq!(dj.order_set(&tracks).await, Ok(vec![2, 0, 1, 3]));
    }

    #[tokio::test]
    async fn order_set_says_why_when_it_cannot() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(chat_response("I would start with the slow ones."))
            .mount(&server)
            .await;
        let dj = Dj::new(
            DjSettings {
                base_url: server.uri(),
                model: "m".into(),
                ..Default::default()
            },
            direct_http(),
        );
        let e = dj
            .order_set(&[("t".into(), "a".into())])
            .await
            .expect_err("no list");
        assert!(e.contains("not a list of track numbers"), "{e}");
        let none = Dj::new(DjSettings::default(), direct_http());
        assert!(none.order_set(&[]).await.is_err());
    }
}
