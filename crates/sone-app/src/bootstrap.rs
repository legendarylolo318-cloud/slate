//! Process bootstrap: logging, async runtime, renderer selection, main window.

use anyhow::Context;
use slint::ComponentHandle;
use std::time::{Duration, Instant};

slint::include_modules!();

use crate::backend::Backend;

/// A failure this early means the renderer could not drive the window at all.
const RENDERER_STARTUP_WINDOW: Duration = Duration::from_secs(5);
const INDEX_ENV: &str = "SONE_RENDERER_INDEX";

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
        .thread_name("sone-rt")
        .build()
        .context("starting tokio runtime")?;
    let _guard = runtime.enter();

    let window = AppWindow::new().context("creating main window")?;
    window.show().context("showing main window")?;
    tracing::info!(renderer = %active, "UI renderer active");

    let backend = Backend::start(runtime.handle().clone(), &window);
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
    let b = backend.clone();
    window.on_start_login(move || b.start_login());
    let b = backend.clone();
    window.on_open_login_url(move || b.open_login_url());
    let b = backend.clone();
    window.on_logout(move || b.logout());
    let b = backend.clone();
    window.on_play_track(move |text| b.play_track(&text));
    let b = backend.clone();
    window.on_toggle_play(move || b.toggle_play());
    let b = backend.clone();
    window.on_next(move || b.next());
    let b = backend.clone();
    window.on_previous(move || b.previous());
    let b = backend.clone();
    window.on_toggle_shuffle(move || b.toggle_shuffle());
    let b = backend.clone();
    window.on_cycle_repeat(move || b.cycle_repeat());
    let b = backend.clone();
    window.on_seek(move |f| b.seek(f));
    let b = backend.clone();
    window.on_set_volume(move |v| b.set_volume(v));
}

/// Slint fixes its platform once per process, so trying another renderer needs
/// a fresh process. The launch-time scrub removed the user's proxy variables
/// from this process; hand them back so the child scrubs from the same input.
fn respawn(next_index: usize) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("locating own executable")?;
    let status = std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .envs(sone_core::proxy::scrubbed_env().iter().cloned())
        .env(INDEX_ENV, next_index.to_string())
        .status()
        .context("relaunching with a fallback renderer")?;
    std::process::exit(status.code().unwrap_or(1));
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("SONE_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
