//! Screenshots of the real window, rendered offscreen with the software renderer, at several
//! window sizes and scale factors. Run on demand:
//!
//! ```text
//! SLATE_SHOTS=settings cargo test -p slate --release ui_shots -- --ignored --nocapture
//! ```
//!
//! PNGs land in `target/ui-shots` (or `$SLATE_SHOTS_DIR`), named `<page>-<w>x<h>@<scale>.png`
//! with the logical window size.

use std::rc::Rc;

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, PhysicalSize};

use crate::bootstrap::{AppWindow, Screen, SettingsState};

/// Logical window sizes: full HD maximised, half of a 1920 screen, a small window, the
/// narrowest that still shows the sidebar, and phone-like narrow.
const SIZES: &[(u32, u32)] = &[
    (1880, 1000),
    (960, 1000),
    (800, 640),
    (640, 600),
    (480, 720),
];
const SCALES: &[f32] = &[1.0, 1.5, 2.0];

struct Offscreen(Rc<MinimalSoftwareWindow>);

/// Offscreen time, in milliseconds: moved forward before each capture so animations finish.
static CLOCK_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }

    fn duration_since_start(&self) -> std::time::Duration {
        std::time::Duration::from_millis(CLOCK_MS.load(std::sync::atomic::Ordering::Relaxed))
    }
}

/// Lets every running animation and timer finish.
fn settle() {
    for _ in 0..4 {
        CLOCK_MS.fetch_add(1000, std::sync::atomic::Ordering::Relaxed);
        slint::platform::update_timers_and_animations();
    }
}

fn save(window: &MinimalSoftwareWindow, w: u32, h: u32, path: &std::path::Path) {
    let mut buf = vec![PremultipliedRgbaColor::default(); (w * h) as usize];
    settle();
    window.request_redraw();
    window.draw_if_needed(|r| {
        r.render(&mut buf, w as usize);
    });
    let bytes: Vec<u8> = buf
        .iter()
        .flat_map(|p| [p.red, p.green, p.blue, 255])
        .collect();
    if let Some(img) = image::RgbaImage::from_raw(w, h, bytes) {
        if let Err(e) = img.save(path) {
            eprintln!("{}: {e}", path.display());
        }
    }
}

#[test]
#[ignore = "writes screenshots; run on demand"]
fn ui_shots() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    if slint::platform::set_platform(Box::new(Offscreen(window.clone()))).is_err() {
        return;
    }
    let Ok(app) = AppWindow::new() else { return };
    let dir = std::env::var_os("SLATE_SHOTS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-shots")
        });
    let _ = std::fs::create_dir_all(&dir);
    let which = std::env::var("SLATE_SHOTS").unwrap_or_else(|_| "settings".into());
    app.set_screen(Screen::Main);
    let _ = app.show();
    // Settings tabs, or the plain track page.
    let pages: Vec<(String, Option<i32>)> = if which == "settings" {
        (0..9).map(|t| (format!("settings{t}"), Some(t))).collect()
    } else {
        vec![(which.clone(), None)]
    };
    for &scale in SCALES {
        window.dispatch_event(WindowEvent::ScaleFactorChanged {
            scale_factor: scale,
        });
        for &(lw, lh) in SIZES {
            let (w, h) = ((lw as f32 * scale) as u32, (lh as f32 * scale) as u32);
            window.set_size(PhysicalSize::new(w, h));
            for (name, tab) in &pages {
                match tab {
                    Some(t) => {
                        app.set_view("settings".into());
                        app.global::<SettingsState>().set_tab(*t);
                    }
                    None => app.set_view(which.as_str().into()),
                }
                save(
                    &window,
                    w,
                    h,
                    &dir.join(format!("{name}-{lw}x{lh}@{scale}.png")),
                );
            }
        }
    }
    eprintln!("screenshots in {}", dir.display());
}
