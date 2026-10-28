//! Process bootstrap: logging, async runtime, renderer selection, main window.

use anyhow::Context;
use slint::ComponentHandle;
use std::time::{Duration, Instant};

slint::include_modules!();

use crate::backend::Backend;

/// A failure this early means the renderer could not drive the window at all.
const RENDERER_STARTUP_WINDOW: Duration = Duration::from_secs(5);
const INDEX_ENV: &str = "SLATE_RENDERER_INDEX";

#[derive(Clone, Copy, Debug)]
enum Renderer {
    SkiaVulkan,
    SkiaOpenGl,
    FemtoVg,
    Software,
}

impl Renderer {
    const fn name(self) -> &'static str {
        match self {
            Self::SkiaVulkan => "skia-vulkan",
            Self::SkiaOpenGl => "skia-opengl",
            Self::FemtoVg => "femtovg",
            Self::Software => "software",
        }
    }

    fn select(self) -> Result<(), slint::PlatformError> {
        let sel = slint::BackendSelector::new().backend_name("winit".into());
        match self {
            Self::SkiaVulkan => sel.renderer_name("skia".into()).require_vulkan(),
            Self::SkiaOpenGl => sel.renderer_name("skia-opengl".into()),
            Self::FemtoVg => sel.renderer_name("femtovg".into()),
            Self::Software => sel.renderer_name("software".into()),
        }
        .select()
    }
}

/// Default → most compatible. Which entries exist depends on Cargo features.
const CHAIN: &[Renderer] = &[
    #[cfg(feature = "renderer-skia-vulkan")]
    Renderer::SkiaVulkan,
    #[cfg(feature = "renderer-skia")]
    Renderer::SkiaOpenGl,
    #[cfg(feature = "renderer-femtovg")]
    Renderer::FemtoVg,
    #[cfg(feature = "renderer-software")]
    Renderer::Software,
];

pub fn run() -> anyhow::Result<()> {
    init_tracing();

    // `SLINT_BACKEND` wins: hand the decision to Slint untouched.
    let forced = std::env::var("SLINT_BACKEND").ok();
    let index: usize = std::env::var(INDEX_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let started = Instant::now();
    let outcome = launch(forced.as_deref(), index);

    match outcome {
        Err(e) if forced.is_none() && started.elapsed() < RENDERER_STARTUP_WINDOW => {
            match CHAIN.get(index + 1) {
                Some(next) => {
                    tracing::warn!(
                        failed = CHAIN.get(index).map_or("?", |r| r.name()),
                        next = next.name(),
                        error = %format!("{e:#}"),
                        "renderer failed during startup; retrying with the next one"
                    );
                    respawn(index + 1)
                }
                None => Err(e.context("every renderer failed to start")),
            }
        }
        other => other,
    }
}

fn launch(forced: Option<&str>, index: usize) -> anyhow::Result<()> {
    let active = match forced {
        Some(name) => {
            tracing::info!(backend = name, "SLINT_BACKEND override in effect");
            name.to_string()
        }
        None => {
            let renderer = *CHAIN.get(index).context("no renderer compiled in")?;
            renderer
                .select()
                .map_err(|e| anyhow::anyhow!("selecting {}: {e}", renderer.name()))?;
            renderer.name().to_string()
        }
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("slate-rt")
        .build()
        .context("starting tokio runtime")?;
    let _guard = runtime.enter();

    let window = AppWindow::new().context("creating main window")?;
    window.show().context("showing main window")?;
    tracing::info!(renderer = %active, "UI renderer active");

    let initial_theme = crate::themes::load_saved();
    crate::themes::apply(&window, &initial_theme);
    let backend = Backend::start(runtime.handle().clone(), &window, initial_theme);
    wire(&window, &backend);
    let b = backend.clone();
    window
        .window()
        .on_close_requested(move || b.close_requested());
    // Not `window.run()`: hiding the window to the tray must not end the loop.
    slint::run_event_loop_until_quit().context("running event loop")?;
    backend.shutdown();
    Ok(())
}

fn wire(window: &AppWindow, backend: &Backend) {
    macro_rules! bind {
        ($on:ident, |$b:ident $(, $a:ident)*| $body:expr) => {{
            let $b = backend.clone();
            window.$on(move |$($a),*| $body);
        }};
    }
    bind!(on_start_login, |b| b.start_login());
    bind!(on_open_login_url, |b| b.open_login_url());
    bind!(on_logout, |b| b.logout());
    bind!(on_back, |b| b.back());
    bind!(on_forward, |b| b.forward());
    bind!(on_go, |b, key| b.go(&key));
    bind!(on_open_playlist, |b, id| b.open_playlist(&id));
    bind!(on_open_card, |b, kind, id| b.open_card(&kind, &id));
    bind!(on_play_card, |b, kind, id| b.play_card(&kind, &id));
    bind!(on_search, |b, q| b.search(&q));
    bind!(on_play_row, |b, i| b
        .play_row(usize::try_from(i).unwrap_or(0)));
    bind!(on_row_action, |b, i, a| b
        .row_action(usize::try_from(i).unwrap_or(0), &a));
    bind!(on_play_all, |b| b.play_all(false));
    bind!(on_shuffle_all, |b| b.play_all(true));
    bind!(on_hero_play, |b| b.hero_play(false));
    bind!(on_hero_shuffle, |b| b.hero_play(true));
    bind!(on_queue_play, |b, i| b
        .queue_play(usize::try_from(i).unwrap_or(0)));
    bind!(on_queue_remove, |b, i| b
        .queue_remove(usize::try_from(i).unwrap_or(0)));
    bind!(on_queue_clear, |b| b.queue_clear());
    bind!(on_lyric_seek, |b, i| b
        .lyric_seek(usize::try_from(i).unwrap_or(0)));
    bind!(on_toggle_favorite, |b| b.toggle_current_favorite());
    bind!(on_toggle_page_favorite, |b| b.toggle_page_favorite());
    bind!(on_toggle_hero_favorite, |b| b.toggle_hero_favorite());
    bind!(on_add_to_playlist, |b, t, u| b.add_to_playlist(t, &u));
    bind!(on_create_playlist, |b, t, n| b.create_playlist(t, &n));
    bind!(on_create_folder, |b, n| b.create_folder(&n));
    bind!(on_dialog_accepted, |b, k, t, x| b
        .dialog_accepted(&k, &t, &x));
    bind!(on_open_signal_path, |b| b.open_signal_path());
    bind!(on_open_miniplayer, |b| b.open_miniplayer());
    bind!(on_toggle_play, |b| b.toggle_play());
    bind!(on_next, |b| b.next());
    bind!(on_previous, |b| b.previous());
    bind!(on_toggle_shuffle, |b| b.toggle_shuffle());
    bind!(on_cycle_repeat, |b| b.cycle_repeat());
    bind!(on_toggle_mute, |b| b.toggle_mute());
    bind!(on_seek, |b, f| b.seek(f));
    bind!(on_seek_relative, |b, d| b.seek_relative(d));
    bind!(on_set_volume, |b, v| b.set_volume(v));

    let g = window.global::<SettingsState>();
    let b = backend.clone();
    g.on_theme_preset(move |i| b.theme_preset(usize::try_from(i).unwrap_or(0)));
    let b = backend.clone();
    g.on_theme_color(move |t, c| b.theme_color(t, c, false));
    let b = backend.clone();
    g.on_theme_color_final(move |t, c| b.theme_color(t, c, true));
    let b = backend.clone();
    g.on_theme_hex(move |t, s| b.theme_hex(t, &s));
    let b = backend.clone();
    g.on_changed_bool(move |k, v| b.setting_bool(&k, v));
    let b = backend.clone();
    g.on_changed_int(move |k, v| b.setting_int(&k, v));
    let b = backend.clone();
    g.on_changed_str(move |k, v| b.setting_str(&k, &v));
    let b = backend.clone();
    g.on_action(move |a| b.setting_action(&a));
}

/// Slint fixes its platform once per process, so trying another renderer needs
/// a fresh process. The launch-time scrub removed the user's proxy variables
/// from this process; hand them back so the child scrubs from the same input.
fn respawn(next_index: usize) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("locating own executable")?;
    let status = std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .envs(slate_core::proxy::scrubbed_env().iter().cloned())
        .env(INDEX_ENV, next_index.to_string())
        .status()
        .context("relaunching with a fallback renderer")?;
    std::process::exit(status.code().unwrap_or(1));
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("SLATE_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
