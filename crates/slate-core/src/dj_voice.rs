//! The DJ's Piper voice. Nothing exists until a line is spoken: then `piper` (text on stdin
//! only, never an argument) writes raw audio to a short-lived player (`paplay` or `aplay` on
//! Linux, a one-off PowerShell `SoundPlayer` on Windows). Both processes are killed when a new
//! line or a track change interrupts, and the music volume is ducked while it speaks.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Called with `true` when speech starts and `false` when it ends (however it ends).
pub type Duck = Arc<dyn Fn(bool) + Send + Sync>;

/// Sample rate when the model's `.onnx.json` does not say.
const DEFAULT_RATE: u32 = 22_050;
/// A line takes at most this long, so a stuck process cannot hold the music down.
const MAX_SPEECH: Duration = Duration::from_secs(60);

static GENERATION: AtomicU64 = AtomicU64::new(0);
/// The generation of the line that owns `SPEAKING`.
static ACTIVE: AtomicU64 = AtomicU64::new(0);
static SPEAKING: AtomicBool = AtomicBool::new(false);
/// The next track start belongs to the line just spoken and must not interrupt it.
static SKIP_NEXT_START: AtomicBool = AtomicBool::new(false);
static CHILDREN: Mutex<Vec<Child>> = Mutex::new(Vec::new());

fn children() -> std::sync::MutexGuard<'static, Vec<Child>> {
    CHILDREN.lock().unwrap_or_else(|p| p.into_inner())
}

/// Which voice reads the DJ's lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Voice {
    /// espeak-ng / say (the original behaviour).
    #[default]
    System,
    Piper,
}

impl Voice {
    pub fn parse(s: &str) -> Self {
        if s.eq_ignore_ascii_case("piper") {
            Self::Piper
        } else {
            Self::System
        }
    }
}

fn find_in(names: &[&str], path: &OsStr) -> Option<PathBuf> {
    names.iter().find_map(|n| {
        std::env::split_paths(path).find_map(|d| {
            [d.join(n), d.join(format!("{n}.exe"))]
                .into_iter()
                .find(|p| p.is_file())
        })
    })
}

/// Why Piper cannot speak, in words for the DJ page; `Ok` when it can.
pub fn piper_status(model: &str, path: &OsStr) -> Result<(), String> {
    if find_in(&["piper"], path).is_none() {
        return Err(
            "Piper is not installed: put the `piper` program on your PATH (pip install piper-tts, or a release from github.com/rhasspy/piper).".into(),
        );
    }
    if model.trim().is_empty() {
        return Err("Choose a Piper voice: a .onnx model file.".into());
    }
    let name = Path::new(model)
        .file_name()
        .map_or_else(|| model.to_string(), |n| n.to_string_lossy().into_owned());
    if !Path::new(model).is_file() {
        return Err(format!("The Piper voice file was not found: {name}"));
    }
    #[cfg(not(windows))]
    if find_in(&["paplay", "aplay"], path).is_none() {
        return Err("Piper needs paplay or aplay to play its audio; neither was found.".into());
    }
    Ok(())
}

/// The sample rate in a model's `.onnx.json` (`{"audio":{"sample_rate":22050}}`).
pub fn sample_rate_of(json: &str) -> u32 {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("audio")?.get("sample_rate")?.as_u64())
        .and_then(|r| u32::try_from(r).ok())
        .filter(|r| (8_000..=96_000).contains(r))
        .unwrap_or(DEFAULT_RATE)
}

fn model_rate(model: &str) -> u32 {
    std::fs::read_to_string(format!("{model}.json")).map_or(DEFAULT_RATE, |s| sample_rate_of(&s))
}

/// True while a Piper line is being spoken.
pub fn is_speaking() -> bool {
    SPEAKING.load(Ordering::Relaxed)
}

/// A track started. Stops a line that is still being spoken, except for the track the
/// line itself introduced. One atomic read when nothing is speaking.
pub fn track_changed() {
    if !SPEAKING.load(Ordering::Relaxed) {
        return;
    }
    if SKIP_NEXT_START.swap(false, Ordering::AcqRel) {
        return;
    }
    interrupt();
}

/// Kills whatever is speaking now.
pub fn interrupt() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
    for c in children().iter_mut() {
        let _ = c.kill();
    }
}

/// Speaks `line` with Piper on a short-lived thread. A line already being spoken is cut
/// off. Errors (missing binary or model, a failing process) are returned only for the
/// checks done before the thread starts; a later failure is logged.
pub fn speak_piper(
    line: &str,
    model: &str,
    duck: Option<Duck>,
    path: Option<OsString>,
) -> Result<(), String> {
    let path = path
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();
    piper_status(model, &path)?;
    let Some(text) = crate::dj::speakable(line) else {
        return Ok(());
    };
    interrupt();
    let generation = GENERATION.load(Ordering::Acquire);
    SKIP_NEXT_START.store(true, Ordering::Release);
    ACTIVE.store(generation, Ordering::Release);
    let model = model.to_string();
    std::thread::Builder::new()
        .name("slate-dj-voice".into())
        .spawn(move || {
            SPEAKING.store(true, Ordering::Relaxed);
            if let Some(d) = &duck {
                d(true);
            }
            if let Err(e) = run(&text, &model, &path, generation) {
                log::warn!("[dj] Piper voice failed: {e}");
            }
            children().clear();
            // A newer line owns the flag now.
            if ACTIVE.load(Ordering::Acquire) == generation {
                SPEAKING.store(false, Ordering::Relaxed);
                SKIP_NEXT_START.store(false, Ordering::Relaxed);
            }
            if let Some(d) = &duck {
                d(false);
            }
        })
        .map_err(|e| format!("could not start the voice: {e}"))?;
    Ok(())
}

fn run(text: &str, model: &str, path: &OsStr, generation: u64) -> Result<(), String> {
    let rate = model_rate(model);
    let piper = find_in(&["piper"], path).ok_or("piper vanished")?;
    let mut tts = Command::new(piper);
    tts.arg("--model")
        .arg(model)
        .stdin(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    let wav = std::env::temp_dir().join(format!("slate-dj-{}.wav", std::process::id()));
    #[cfg(windows)]
    {
        tts.arg("--output_file").arg(&wav).stdout(Stdio::null());
    }
    #[cfg(not(windows))]
    {
        tts.arg("--output_raw").stdout(Stdio::piped());
    }
    let mut tts = tts.spawn().map_err(|e| format!("starting piper: {e}"))?;
    let mut input = tts.stdin.take().ok_or("no stdin")?;
    #[cfg(not(windows))]
    let audio = tts.stdout.take().ok_or("no stdout")?;
    // Register before anything blocks, so an interrupt can reach it.
    children().push(tts);
    if GENERATION.load(Ordering::Acquire) != generation {
        return Ok(());
    }
    // The text goes in on stdin; closing it lets piper finish.
    input
        .write_all(text.as_bytes())
        .and_then(|()| input.write_all(b"\n"))
        .map_err(|e| format!("writing the line: {e}"))?;
    drop(input);

    #[cfg(not(windows))]
    {
        let (player, args): (PathBuf, Vec<String>) = match find_in(&["paplay"], path) {
            Some(p) => (
                p,
                vec![
                    "--raw".into(),
                    format!("--rate={rate}"),
                    "--format=s16le".into(),
                    "--channels=1".into(),
                ],
            ),
            None => (
                find_in(&["aplay"], path).ok_or("no player")?,
                vec![
                    "-q".into(),
                    "-t".into(),
                    "raw".into(),
                    "-r".into(),
                    rate.to_string(),
                    "-f".into(),
                    "S16_LE".into(),
                    "-c".into(),
                    "1".into(),
                ],
            ),
        };
        let play = Command::new(player)
            .args(args)
            .stdin(Stdio::from(audio))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("starting the player: {e}"))?;
        children().push(play);
    }
    #[cfg(windows)]
    {
        let _ = rate;
        wait_all(generation);
        if GENERATION.load(Ordering::Acquire) != generation {
            let _ = std::fs::remove_file(&wav);
            return Ok(());
        }
        // One-off playback of the finished file; the path is an argument, the text never is.
        let play = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg("(New-Object Media.SoundPlayer $args[0]).PlaySync()")
            .arg(&wav)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("starting the player: {e}"))?;
        children().push(play);
        wait_all(generation);
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        wait_all(generation);
        Ok(())
    }
}

/// Waits (polling, only while a line is spoken) until every registered process has ended,
/// the line was interrupted or [`MAX_SPEECH`] passed.
fn wait_all(generation: u64) {
    let start = std::time::Instant::now();
    loop {
        if GENERATION.load(Ordering::Acquire) != generation {
            return;
        }
        let done = children()
            .iter_mut()
            .all(|c| !matches!(c.try_wait(), Ok(None)));
        if done {
            return;
        }
        if start.elapsed() > MAX_SPEECH {
            interrupt();
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    static SERIAL: Mutex<()> = Mutex::new(());

    fn script(dir: &Path, name: &str, body: &str) {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).expect("script");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    fn wait_for(f: impl Fn() -> bool) -> bool {
        for _ in 0..200 {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }

    #[test]
    fn status_names_what_is_missing() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().as_os_str();
        let e = piper_status("", path).expect_err("no piper");
        assert!(e.contains("Piper is not installed"), "{e}");
        script(dir.path(), "piper", "");
        let e = piper_status("", path).expect_err("no model");
        assert!(e.contains("Choose a Piper voice"), "{e}");
        let e = piper_status("/nowhere/amy.onnx", path).expect_err("model missing");
        assert!(e.contains("amy.onnx"), "{e}");
        let model = dir.path().join("amy.onnx");
        std::fs::write(&model, b"x").expect("model");
        let e = piper_status(model.to_str().expect("utf8"), path).expect_err("no player");
        assert!(e.contains("paplay or aplay"), "{e}");
        script(dir.path(), "aplay", "");
        assert_eq!(piper_status(model.to_str().expect("utf8"), path), Ok(()));
    }

    #[test]
    fn rates_come_from_the_model_file() {
        assert_eq!(sample_rate_of(r#"{"audio":{"sample_rate":16000}}"#), 16_000);
        assert_eq!(sample_rate_of(r#"{"audio":{}}"#), DEFAULT_RATE);
        assert_eq!(sample_rate_of("nope"), DEFAULT_RATE);
        assert_eq!(
            sample_rate_of(r#"{"audio":{"sample_rate":5}}"#),
            DEFAULT_RATE
        );
        assert_eq!(Voice::parse("Piper"), Voice::Piper);
        assert_eq!(Voice::parse(""), Voice::System);
        assert_eq!(Voice::parse("espeak"), Voice::System);
    }

    #[test]
    fn the_line_goes_to_piper_on_stdin_and_its_audio_to_the_player() {
        let _s = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().expect("dir");
        let out = dir.path().join("out");
        std::fs::create_dir(&out).expect("out");
        let o = out.display();
        script(
            dir.path(),
            "piper",
            &format!("echo \"$@\" > {o}/args; cat > {o}/text; printf AUDIO"),
        );
        script(
            dir.path(),
            "paplay",
            &format!("echo \"$@\" > {o}/pargs; cat > {o}/audio"),
        );
        let model = dir.path().join("v.onnx");
        std::fs::write(&model, b"x").expect("model");
        std::fs::write(
            dir.path().join("v.onnx.json"),
            r#"{"audio":{"sample_rate":16000}}"#,
        )
        .expect("json");
        let ducks = Arc::new(Mutex::new(Vec::new()));
        let d = Arc::clone(&ducks);
        let duck: Duck = Arc::new(move |on| d.lock().expect("ducks").push(on));
        let m = model.to_str().expect("utf8");
        speak_piper(
            "--rm -rf $(whoami) hello there",
            m,
            Some(duck),
            Some(dir.path().as_os_str().to_owned()),
        )
        .expect("speaks");
        assert!(wait_for(|| out.join("audio").exists()
            && std::fs::read_to_string(out.join("audio"))
                .is_ok_and(|s| s == "AUDIO")));
        assert!(wait_for(|| ducks.lock().expect("ducks").len() == 2));
        assert_eq!(*ducks.lock().expect("ducks"), vec![true, false]);
        let text = std::fs::read_to_string(out.join("text")).expect("text");
        assert_eq!(
            text.trim(),
            "rm -rf $(whoami) hello there",
            "leading dashes are dropped"
        );
        let args = std::fs::read_to_string(out.join("args")).expect("args");
        assert!(
            !args.contains("hello") && args.contains("--output_raw"),
            "{args}"
        );
        let pargs = std::fs::read_to_string(out.join("pargs")).expect("pargs");
        assert!(pargs.contains("--rate=16000"), "{pargs}");
        assert!(wait_for(|| !is_speaking()));
    }

    #[test]
    fn a_new_line_or_a_track_change_kills_the_voice_and_restores_the_volume() {
        let _s = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().expect("dir");
        script(dir.path(), "piper", "cat > /dev/null; sleep 30");
        script(dir.path(), "aplay", "cat > /dev/null");
        let model = dir.path().join("v.onnx");
        std::fs::write(&model, b"x").expect("model");
        let m = model.to_str().expect("utf8");
        let ducks = Arc::new(Mutex::new(Vec::new()));
        let d = Arc::clone(&ducks);
        let duck: Duck = Arc::new(move |on| d.lock().expect("ducks").push(on));
        let path = dir.path().as_os_str().to_owned();
        speak_piper("first", m, Some(Arc::clone(&duck)), Some(path.clone())).expect("speaks");
        assert!(wait_for(is_speaking));
        // The track the line introduced does not cut it off ...
        track_changed();
        std::thread::sleep(Duration::from_millis(150));
        assert!(is_speaking(), "the introduced track keeps the line going");
        // ... the next track change does.
        let started = std::time::Instant::now();
        track_changed();
        assert!(wait_for(|| !is_speaking()));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "killed, not waited out"
        );
        assert!(wait_for(
            || ducks.lock().expect("ducks").last() == Some(&false)
        ));
        // A new line interrupts the old one.
        speak_piper("second", m, Some(duck), Some(path)).expect("speaks");
        assert!(wait_for(is_speaking));
        interrupt();
        assert!(wait_for(|| !is_speaking()));
    }
}
