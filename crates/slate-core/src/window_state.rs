//! Remembered window geometry: pure rules, no windowing code.
//!
//! The app reads the window (size, position, maximized), calls [`observe`] and stores the
//! result in a [`Store`]; at launch [`plan`] turns a stored [`Geom`] and the current monitor
//! layout into what to apply. Sizes are logical (they follow the interface scale and
//! per-monitor DPI); positions are physical desktop coordinates, which is what monitors are
//! listed in.

use serde::{Deserialize, Serialize};

/// A rectangle in physical desktop pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// What is remembered of one window.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Geom {
    /// Logical size of the restored (not maximized) window.
    pub w: f32,
    pub h: f32,
    /// Physical top-left, when the platform reports one (not on Wayland).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<i32>,
    #[serde(default)]
    pub maximized: bool,
}

/// The stored geometry of both windows (`window.json`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Store {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main: Option<Geom>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mini: Option<Geom>,
}

/// Which window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    Main,
    Mini,
}

impl Which {
    /// Smallest logical size worth restoring.
    fn min_size(self) -> f32 {
        match self {
            Self::Main => 320.0,
            Self::Mini => 120.0,
        }
    }
}

/// Positions below this are what Windows reports for a minimized window.
const MINIMIZED_COORD: i32 = -30_000;
const MAX_LOGICAL: f32 = 16_384.0;

impl Store {
    pub fn parse(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_default()
    }

    pub fn get(&self, which: Which) -> Option<Geom> {
        match which {
            Which::Main => self.main,
            Which::Mini => self.mini,
        }
        .and_then(|g| g.sane(which))
    }

    pub fn set(&mut self, which: Which, g: Geom) {
        match which {
            Which::Main => self.main = Some(g),
            Which::Mini => self.mini = Some(g),
        }
    }
}

impl Geom {
    /// `None` for a size that cannot be right (a damaged file); otherwise clamped.
    fn sane(self, which: Which) -> Option<Self> {
        let ok = |v: f32| v.is_finite() && v >= 1.0;
        if !ok(self.w) || !ok(self.h) {
            return None;
        }
        let min = which.min_size();
        Some(Self {
            w: self.w.clamp(min, MAX_LOGICAL),
            h: self.h.clamp(min, MAX_LOGICAL),
            ..self
        })
    }
}

/// What the window reports right now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Observed {
    pub logical_w: f32,
    pub logical_h: f32,
    pub pos: Option<(i32, i32)>,
    pub maximized: bool,
    pub minimized: bool,
    pub fullscreen: bool,
}

/// The geometry to store after seeing `now`, or `None` to leave the stored one alone
/// (minimized, full screen, or nonsense). A maximized window keeps the size and position
/// it had before, so un-maximizing after a restart goes back to where it was.
pub fn observe(prev: Option<Geom>, now: Observed) -> Option<Geom> {
    if now.minimized || now.fullscreen {
        return None;
    }
    if now.maximized {
        // Never saw it un-maximized: the maximized size is no restored size, so remember a
        // smaller window (80 %) to come back to, and no position.
        let base = prev.unwrap_or(Geom {
            w: (now.logical_w * 0.8).floor(),
            h: (now.logical_h * 0.8).floor(),
            x: None,
            y: None,
            maximized: true,
        });
        return Some(Geom {
            maximized: true,
            ..base
        });
    }
    if !(now.logical_w.is_finite() && now.logical_h.is_finite())
        || now.logical_w < 1.0
        || now.logical_h < 1.0
    {
        return None;
    }
    let pos = now
        .pos
        .filter(|(x, y)| *x > MINIMIZED_COORD && *y > MINIMIZED_COORD);
    Some(Geom {
        w: now.logical_w,
        h: now.logical_h,
        x: pos.map(|p| p.0),
        y: pos.map(|p| p.1),
        maximized: false,
    })
}

/// What to apply when a window opens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plan {
    pub w: f32,
    pub h: f32,
    pub maximized: bool,
    /// Physical top-left to move to; `None` leaves placement to the window manager.
    pub pos: Option<(i32, i32)>,
}

/// Turns stored geometry and the monitor layout into a [`Plan`].
///
/// A position that no longer shows the window's title area on any monitor (unplugged
/// screen, lower resolution) is replaced by the centre of the primary monitor. Without a
/// monitor list (not known before the window exists) a stored position is kept and
/// [`needs_recentering`] decides again once it is.
/// `can_position` is false on Wayland, where clients cannot set their position: only size
/// and maximized are restored there.
pub fn plan(
    g: Geom,
    scale: f32,
    monitors: &[Rect],
    primary: Option<Rect>,
    can_position: bool,
) -> Plan {
    let mut p = Plan {
        w: g.w,
        h: g.h,
        maximized: g.maximized,
        pos: None,
    };
    let Some(mon) = primary.or_else(|| monitors.first().copied()) else {
        p.pos = can_position.then_some(g.x.zip(g.y)).flatten();
        return p;
    };
    // Never open larger than the screen it is going to: the one it was on, else the primary.
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let home =
        g.x.zip(g.y)
            .and_then(|(x, y)| {
                monitors
                    .iter()
                    .copied()
                    .find(|m| contains(*m, x + 24, y + 12))
            })
            .unwrap_or(mon);
    p.w = p.w.min(home.w as f32 / scale);
    p.h = p.h.min(home.h as f32 / scale);
    if !can_position {
        return p;
    }
    let size = ((p.w * scale).round() as i32, (p.h * scale).round() as i32);
    p.pos = match g.x.zip(g.y) {
        Some((x, y))
            if title_area_visible(
                Rect {
                    x,
                    y,
                    w: size.0,
                    h: size.1,
                },
                monitors,
            ) =>
        {
            Some((x, y))
        }
        // Nothing stored (first run) or off every monitor: centre on the primary.
        Some(_) => Some(centered(size, mon)),
        None => None,
    };
    p
}

/// Whether a window already placed at `win` is off every monitor and must be moved.
pub fn needs_recentering(win: Rect, monitors: &[Rect]) -> bool {
    !monitors.is_empty() && !title_area_visible(win, monitors)
}

/// The top-left that centres a `size` window on `mon`.
pub fn centered(size: (i32, i32), mon: Rect) -> (i32, i32) {
    (
        mon.x + (mon.w - size.0).max(0) / 2,
        mon.y + (mon.h - size.1).max(0) / 2,
    )
}

/// Enough of the title bar (a 120 × 40 strip at the top-left… and the middle) shows on a
/// monitor to grab the window. Checks the strip's centre point and its left end.
fn title_area_visible(win: Rect, monitors: &[Rect]) -> bool {
    let strip_w = win.w.min(120);
    let probes = [
        (win.x + 24, win.y + 12),
        (win.x + win.w / 2, win.y + 12),
        (win.x + strip_w - 24, win.y + 12),
    ];
    probes
        .iter()
        .any(|&(px, py)| monitors.iter().any(|m| contains(*m, px, py)))
}

fn contains(m: Rect, px: i32, py: i32) -> bool {
    px >= m.x && px < m.x + m.w && py >= m.y && py < m.y + m.h
}

/// Wayland sessions do not let a client place its window.
pub fn positioning_supported(env: impl Fn(&str) -> Option<String>) -> bool {
    let set = |k: &str| env(k).is_some_and(|v| !v.is_empty());
    let forced_x11 = env("WINIT_UNIX_BACKEND").is_some_and(|v| v == "x11");
    !(set("WAYLAND_DISPLAY") && !forced_x11)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(x: i32, w: i32) -> Rect {
        Rect {
            x,
            y: 0,
            w,
            h: 1080,
        }
    }

    fn geom(x: i32, y: i32) -> Geom {
        Geom {
            w: 1000.0,
            h: 700.0,
            x: Some(x),
            y: Some(y),
            maximized: false,
        }
    }

    fn seen(w: f32, h: f32, pos: Option<(i32, i32)>) -> Observed {
        Observed {
            logical_w: w,
            logical_h: h,
            pos,
            maximized: false,
            minimized: false,
            fullscreen: false,
        }
    }

    #[test]
    fn a_position_on_a_monitor_is_kept() {
        let m = [mon(0, 1920)];
        let p = plan(geom(300, 200), 1.0, &m, Some(m[0]), true);
        assert_eq!(p.pos, Some((300, 200)));
        assert_eq!((p.w, p.h), (1000.0, 700.0));
    }

    #[test]
    fn a_position_on_an_unplugged_monitor_centres_on_the_primary() {
        let m = [mon(0, 1920)];
        let p = plan(geom(2500, 100), 1.0, &m, Some(m[0]), true);
        assert_eq!(p.pos, Some((460, 190)));
    }

    #[test]
    fn a_second_monitor_to_the_left_with_negative_coordinates_counts() {
        let m = [mon(-1920, 1920), mon(0, 1920)];
        let p = plan(geom(-1500, 100), 1.0, &m, Some(m[1]), true);
        assert_eq!(p.pos, Some((-1500, 100)));
    }

    #[test]
    fn a_window_mostly_off_screen_but_with_its_title_visible_stays() {
        let m = [mon(0, 1920)];
        let p = plan(geom(1800, 100), 1.0, &m, Some(m[0]), true);
        assert_eq!(p.pos, Some((1800, 100)));
        // Only its far right edge shows: the grab strip is off screen.
        let off = plan(geom(1990, 100), 1.0, &m, Some(m[0]), true);
        assert_eq!(off.pos, Some((460, 190)));
    }

    #[test]
    fn wayland_restores_size_and_maximized_only() {
        let m = [mon(0, 1920)];
        let mut g = geom(300, 200);
        g.maximized = true;
        let p = plan(g, 1.0, &m, Some(m[0]), false);
        assert_eq!(p.pos, None);
        assert!(p.maximized);
        assert_eq!((p.w, p.h), (1000.0, 700.0));
    }

    #[test]
    fn a_window_bigger_than_the_new_screen_is_shrunk_by_the_scale() {
        let m = [Rect {
            x: 0,
            y: 0,
            w: 1280,
            h: 720,
        }];
        let p = plan(geom(0, 0), 2.0, &m, Some(m[0]), true);
        assert_eq!((p.w, p.h), (640.0, 360.0));
        let p = plan(geom(0, 0), 1.0, &m, Some(m[0]), true);
        assert_eq!((p.w, p.h), (1000.0, 700.0));
    }

    #[test]
    fn without_a_monitor_list_the_stored_position_is_kept_for_a_later_check() {
        let p = plan(geom(5000, 5000), 1.0, &[], None, true);
        assert_eq!(p.pos, Some((5000, 5000)));
        assert!(needs_recentering(
            Rect {
                x: 5000,
                y: 5000,
                w: 1000,
                h: 700
            },
            &[mon(0, 1920)]
        ));
        assert!(!needs_recentering(
            Rect {
                x: 10,
                y: 10,
                w: 1000,
                h: 700
            },
            &[mon(0, 1920)]
        ));
        assert!(!needs_recentering(
            Rect {
                x: 5000,
                y: 5000,
                w: 1000,
                h: 700
            },
            &[]
        ));
    }

    #[test]
    fn maximized_keeps_the_size_it_had_when_restored() {
        let before = geom(300, 200);
        let mut now = seen(1920.0, 1080.0, Some((0, 0)));
        now.maximized = true;
        let g = observe(Some(before), now).expect("stored");
        assert!(g.maximized);
        assert_eq!((g.w, g.h, g.x, g.y), (1000.0, 700.0, Some(300), Some(200)));
        let g = observe(Some(g), seen(900.0, 600.0, Some((50, 60)))).expect("stored");
        assert!(!g.maximized);
        assert_eq!((g.w, g.x), (900.0, Some(50)));
    }

    #[test]
    fn maximized_with_no_earlier_size_remembers_a_smaller_window() {
        let mut now = seen(1920.0, 1080.0, Some((0, 0)));
        now.maximized = true;
        let g = observe(None, now).expect("stored");
        assert!(g.maximized);
        assert_eq!((g.w, g.h, g.x, g.y), (1536.0, 864.0, None, None));
    }

    #[test]
    fn minimized_and_fullscreen_windows_store_nothing() {
        let mut now = seen(10.0, 10.0, Some((-32000, -32000)));
        now.minimized = true;
        assert_eq!(observe(None, now), None);
        let mut now = seen(1920.0, 1080.0, Some((0, 0)));
        now.fullscreen = true;
        assert_eq!(observe(None, now), None);
        // A Windows-style minimized position without the flag is dropped, size is kept.
        let g = observe(None, seen(800.0, 600.0, Some((-32000, -32000)))).expect("stored");
        assert_eq!((g.x, g.y), (None, None));
    }

    #[test]
    fn damaged_files_and_sizes_are_handled() {
        assert_eq!(Store::parse("not json"), Store::default());
        let s = Store::parse(r#"{"main":{"w":-5,"h":700}}"#);
        assert_eq!(s.get(Which::Main), None);
        let s = Store::parse(r#"{"mini":{"w":10,"h":99999,"maximized":false}}"#);
        let g = s.get(Which::Mini).expect("clamped");
        assert_eq!((g.w, g.h), (120.0, 16384.0));
    }

    #[test]
    fn store_round_trips_and_keeps_windows_apart() {
        let mut s = Store::default();
        s.set(Which::Main, geom(1, 2));
        s.set(
            Which::Mini,
            Geom {
                w: 300.0,
                h: 140.0,
                x: None,
                y: None,
                maximized: false,
            },
        );
        let back = Store::parse(&serde_json::to_string(&s).expect("json"));
        assert_eq!(back, s);
        assert_eq!(back.get(Which::Mini).map(|g| g.w), Some(300.0));
    }

    #[test]
    fn wayland_is_detected_from_the_environment() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| (*v).to_string())
            }
        };
        assert!(positioning_supported(env(&[])));
        assert!(!positioning_supported(env(&[(
            "WAYLAND_DISPLAY",
            "wayland-0"
        )])));
        assert!(positioning_supported(env(&[
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("WINIT_UNIX_BACKEND", "x11")
        ])));
        assert!(positioning_supported(env(&[("WAYLAND_DISPLAY", "")])));
    }
}
