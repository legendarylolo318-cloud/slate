//! Process bootstrap: logging, async runtime, renderer selection, main window.

use anyhow::Context;
use slint::ComponentHandle;

slint::include_modules!();

pub fn run() -> anyhow::Result<()> {
    init_tracing();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("sone-rt")
        .build()
        .context("starting tokio runtime")?;
    let _guard = runtime.enter();

    let renderer = select_renderer()?;
    tracing::info!(%renderer, "UI renderer active");

    let window = AppWindow::new().context("creating main window")?;
    window.set_status(format!("SONE — renderer: {renderer}").into());
    window.run().context("running event loop")?;
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("SONE_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

/// Picks a renderer, honouring `SLINT_BACKEND` and otherwise walking the
/// fallback chain Skia/Vulkan → Skia/OpenGL → FemtoVG → software.
fn select_renderer() -> anyhow::Result<String> {
    if let Ok(forced) = std::env::var("SLINT_BACKEND") {
        tracing::info!(%forced, "SLINT_BACKEND override in effect");
        return Ok(forced);
    }
    let chain: &[&str] = &[
        #[cfg(feature = "renderer-skia-vulkan")]
        "skia-vulkan",
        #[cfg(feature = "renderer-skia")]
        "skia-opengl",
        #[cfg(feature = "renderer-femtovg")]
        "femtovg",
        #[cfg(feature = "renderer-software")]
        "software",
    ];
    for name in chain {
        let attempt = slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name((*name).into())
            .select();
        match attempt {
            Ok(()) => return Ok((*name).to_string()),
            Err(e) => {
                tracing::warn!(renderer = name, error = %e, "renderer unavailable, falling back")
            }
        }
    }
    anyhow::bail!("no usable Slint renderer")
}
