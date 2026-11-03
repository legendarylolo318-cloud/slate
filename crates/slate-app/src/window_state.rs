//! Remember window size, position and maximized state (main window and mini player).
//!
//! The rules are in `slate_core::window_state`; this is the windowing glue. Nothing runs
//! while the window is idle: a move or resize restarts one single-shot debounce timer, and
//! only when it fires is the geometry read (UI thread, a few getters) and handed to the
//! runtime's blocking pool to be written to `window.json`. The file is read once at launch.
//!
//! Wayland: clients cannot read or set their position, so only size and maximized are
//! restored there (`positioning_supported`).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use slate_core::window_state::{
    needs_recentering, observe, plan, positioning_supported, Geom, Observed, Rect, Store, Which,
};
use slint::winit_030::WinitWindowAccessor;
use slint::{LogicalSize, PhysicalPosition};

/// How long a move or resize must stop before it is saved.
const DEBOUNCE: Duration = Duration::from_millis(800);

static ENABLED: AtomicBool = AtomicBool::new(true);
/// Serialises read-modify-write of the file between the two windows' saves.
static FILE: Mutex<()> = Mutex::new(());

fn index(which: Which) -> usize {
    match which {
        Which::Main => 0,
        Which::Mini => 1,
    }
}

type Capture = Rc<dyn Fn()>;

thread_local! {
    static TIMERS: RefCell<[slint::Timer; 2]> = RefCell::new([slint::Timer::default(), slint::Timer::default()]);
    static CAPTURES: RefCell<[Option<Capture>; 2]> = const { RefCell::new([None, None]) };
    /// The last geometry stored per window (a maximized window keeps its restored size).
    static LAST: RefCell<[Option<Geom>; 2]> = const { RefCell::new([None, None]) };
    static STORE: RefCell<Store> = RefCell::new(Store::default());
    /// Whether the on-screen check ran (a window started hidden in the tray is checked when
    /// it is first shown).
    static CHECKED: std::cell::Cell<[bool; 2]> = const { std::cell::Cell::new([false, false]) };
}

fn path() -> Option<PathBuf> {
    slate_core::config_dir_for_env().map(|d| d.join("window.json"))
}

fn can_position() -> bool {
    positioning_supported(|k| std::env::var(k).ok())
}

/// Reads the stored geometry once, at launch (before the first window opens), and applies the
/// "Remember window size and position" setting. Off: nothing is read, and the file is deleted.
pub fn init(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
    if !enabled {
        delete_file();
        return;
    }
    let store = path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| Store::parse(&s))
        .unwrap_or_default();
    LAST.with(|l| *l.borrow_mut() = [store.get(Which::Main), store.get(Which::Mini)]);
    STORE.with(|s| *s.borrow_mut() = store);
}

fn delete_file() {
    if let Some(p) = path() {
        if let Err(e) = std::fs::remove_file(&p) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::debug!("could not delete the stored window geometry: {e}");
            }
        }
    }
}

/// The setting was switched in Settings → General (called off the UI thread).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if !on {
        let _guard = FILE.lock().unwrap_or_else(|p| p.into_inner());
        delete_file();
    }
}

/// Applies the stored size, maximized state and position of `which` to a window that is not
/// shown yet. Cheap: the file was read by [`init`].
pub fn restore(window: &slint::Window, which: Which) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let Some(g) = STORE.with(|s| s.borrow().get(which)) else {
        return;
    };
    window.set_size(LogicalSize::new(g.w, g.h));
    if g.maximized {
        window.set_maximized(true);
    }
    if can_position() {
        if let Some((x, y)) = g.x.zip(g.y) {
            window.set_position(PhysicalPosition::new(x, y));
        }
    }
}

fn monitors_of(window: &slint::Window) -> Option<(Vec<Rect>, Option<Rect>)> {
    window.with_winit_window(|w| {
        let rect = |m: slint::winit_030::winit::monitor::MonitorHandle| {
            let (p, s) = (m.position(), m.size());
            Rect {
                x: p.x,
                y: p.y,
                w: i32::try_from(s.width).unwrap_or(i32::MAX),
                h: i32::try_from(s.height).unwrap_or(i32::MAX),
            }
        };
        (
            w.available_monitors().map(rect).collect(),
            w.primary_monitor().map(rect),
        )
    })
}

/// [`after_show`] for a window that may have been shown before: runs it the first time only.
pub fn after_first_show(window: &slint::Window, which: Which) {
    if !CHECKED.with(|c| c.get()[index(which)]) {
        after_show(window, which);
    }
}

/// Once the window exists: if its remembered spot is on no monitor (unplugged, resolution
/// change), move it to the centre of the primary monitor; if it is bigger than its screen (a
/// smaller monitor, a larger interface scale), shrink it to fit and centre it there.
pub fn after_show(window: &slint::Window, which: Which) {
    CHECKED.with(|c| {
        let mut v = c.get();
        v[index(which)] = true;
        c.set(v);
    });
    if !ENABLED.load(Ordering::Relaxed) || !can_position() {
        return;
    }
    let Some(g) = STORE.with(|s| s.borrow().get(which)) else {
        return;
    };
    let Some((monitors, primary)) = monitors_of(window) else {
        return;
    };
    let size = window.size();
    let pos = window.position();
    let current = Rect {
        x: pos.x,
        y: pos.y,
        w: i32::try_from(size.width).unwrap_or(i32::MAX),
        h: i32::try_from(size.height).unwrap_or(i32::MAX),
    };
    if window.is_maximized() {
        return;
    }
    let off_screen = needs_recentering(current, &monitors);
    // The screen the window opened on (by its title bar), else the primary.
    let home = monitors
        .iter()
        .copied()
        .find(|m| contains(*m, current.x + 24, current.y + 12))
        .or(primary)
        .or_else(|| monitors.first().copied());
    let too_big = home.is_some_and(|m| current.w > m.w || current.h > m.h);
    if !off_screen && !too_big {
        return;
    }
    let p = plan(g, window.scale_factor(), &monitors, primary, true);
    let target = if off_screen {
        primary.or_else(|| monitors.first().copied())
    } else {
        home
    };
    if let Some(mon) = target {
        // Fit the screen it lands on, whatever the stored size was.
        let scale = window.scale_factor().max(0.1);
        let p = slate_core::window_state::Plan {
            w: p.w.min(mon.w as f32 / scale),
            h: p.h.min(mon.h as f32 / scale),
            ..p
        };
        let (x, y) = slate_core::window_state::centered(
            (
                (p.w * window.scale_factor()).round() as i32,
                (p.h * window.scale_factor()).round() as i32,
            ),
            mon,
        );
        window.set_size(LogicalSize::new(p.w, p.h));
        window.set_position(PhysicalPosition::new(x, y));
    }
}

fn contains(m: Rect, px: i32, py: i32) -> bool {
    px >= m.x && px < m.x + m.w && py >= m.y && py < m.y + m.h
}

fn observed(window: &slint::Window) -> Observed {
    let scale = window.scale_factor().max(0.1);
    let size = window.size();
    let pos = can_position().then(|| window.position());
    Observed {
        logical_w: size.width as f32 / scale,
        logical_h: size.height as f32 / scale,
        pos: pos.map(|p| (p.x, p.y)),
        maximized: window.is_maximized(),
        minimized: window.is_minimized(),
        fullscreen: window.is_fullscreen(),
    }
}

/// Reads `window` now and hands the geometry to the blocking pool to be saved.
pub fn save_now(rt: &tokio::runtime::Handle, window: &slint::Window, which: Which) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let prev = LAST.with(|l| l.borrow()[index(which)]);
    let Some(g) = observe(prev, observed(window)) else {
        return;
    };
    if prev == Some(g) {
        return;
    }
    LAST.with(|l| l.borrow_mut()[index(which)] = Some(g));
    rt.spawn_blocking(move || write(which, g));
}

fn write(which: Which, g: Geom) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let Some(p) = path() else { return };
    let _guard = FILE.lock().unwrap_or_else(|p| p.into_inner());
    // Re-check under the lock: the setting may have been switched off meanwhile.
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let mut store = std::fs::read_to_string(&p)
        .map(|s| Store::parse(&s))
        .unwrap_or_default();
    store.set(which, g);
    let result = (|| -> std::io::Result<()> {
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&p, serde_json::to_string(&store)?)
    })();
    if let Err(e) = result {
        tracing::debug!("could not save the window geometry: {e}");
    }
}

/// Registers how to save `which` (called once per window, UI thread).
pub fn install(which: Which, capture: impl Fn() + 'static) {
    CAPTURES.with(|c| c.borrow_mut()[index(which)] = Some(Rc::new(capture)));
}

/// A move or resize happened: (re)start the debounce. No timer exists while nothing moves.
pub fn touch(which: Which) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let i = index(which);
    TIMERS.with(|t| {
        t.borrow()[i].start(slint::TimerMode::SingleShot, DEBOUNCE, move || {
            if let Some(f) = CAPTURES.with(|c| c.borrow()[i].clone()) {
                f();
            }
        });
    });
}

/// True for the winit events that change geometry.
pub fn is_geometry_event(event: &slint::winit_030::winit::event::WindowEvent) -> bool {
    use slint::winit_030::winit::event::WindowEvent;
    matches!(event, WindowEvent::Moved(_) | WindowEvent::Resized(_))
}
