//! Colours from album art: picks the cover's main colour and makes it a
//! background the theme's text can be read on.
//!
//! Everything here is plain arithmetic on pixels that are already decoded;
//! callers run `dominant` on a blocking thread.

use slint::{Rgba8Pixel, SharedPixelBuffer};

/// WCAG AA for normal-size text.
const AA: f64 = 4.5;
/// Roughly 32 x 32 samples is plenty to find the main colour.
const SAMPLES_PER_SIDE: usize = 32;
/// Hue buckets of 30 degrees.
const HUE_BUCKETS: usize = 12;
/// Pixels this grey or this dark carry no hue worth tinting with.
const MIN_SATURATION: f64 = 0.18;
const MIN_VALUE: f64 = 0.15;
/// A cover where less than this share of samples is colourful stays untinted.
const MIN_COLOURFUL_SHARE: f64 = 0.04;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn from_hex(hex: &str) -> Option<Self> {
        let h = hex.trim().strip_prefix('#')?;
        if h.len() != 6 {
            return None;
        }
        let n = u32::from_str_radix(h, 16).ok()?;
        Some(Self((n >> 16) as u8, (n >> 8) as u8, n as u8))
    }
}

/// (hue 0..360, saturation 0..1, value 0..1)
fn hsv(r: u8, g: u8, b: u8) -> (f64, f64, f64) {
    let (r, g, b) = (
        f64::from(r) / 255.0,
        f64::from(g) / 255.0,
        f64::from(b) / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let s = if max == 0.0 { 0.0 } else { d / max };
    (h, s, max)
}

/// The cover's main non-grey colour, or `None` for a grey, dark or blank cover.
pub fn dominant(px: &SharedPixelBuffer<Rgba8Pixel>) -> Option<Rgb> {
    let (w, h) = (px.width() as usize, px.height() as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let data = px.as_slice();
    let step = (w.max(h) / SAMPLES_PER_SIDE).max(1);
    // Per hue bucket: total weight and weight-summed channels.
    let mut buckets = [(0.0f64, [0.0f64; 3]); HUE_BUCKETS];
    let mut sampled = 0usize;
    let mut colourful = 0usize;
    for y in (0..h).step_by(step) {
        for x in (0..w).step_by(step) {
            let p = data[y * w + x];
            if p.a < 128 {
                continue;
            }
            sampled += 1;
            let (hue, s, v) = hsv(p.r, p.g, p.b);
            if s < MIN_SATURATION || v < MIN_VALUE {
                continue;
            }
            colourful += 1;
            // Vivid, bright pixels speak loudest.
            let weight = s * v;
            let i = ((hue / 30.0) as usize).min(HUE_BUCKETS - 1);
            buckets[i].0 += weight;
            buckets[i].1[0] += weight * f64::from(p.r);
            buckets[i].1[1] += weight * f64::from(p.g);
            buckets[i].1[2] += weight * f64::from(p.b);
        }
    }
    if sampled == 0 || (colourful as f64) < MIN_COLOURFUL_SHARE * sampled as f64 {
        return None;
    }
    let (weight, sums) = buckets.iter().copied().max_by(|a, b| a.0.total_cmp(&b.0))?;
    if weight <= 0.0 {
        return None;
    }
    let ch = |i: usize| (sums[i] / weight).round().clamp(0.0, 255.0) as u8;
    Some(Rgb(ch(0), ch(1), ch(2)))
}

fn linear(c: u8) -> f64 {
    let c = f64::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn luminance(c: Rgb) -> f64 {
    0.2126 * linear(c.0) + 0.7152 * linear(c.1) + 0.0722 * linear(c.2)
}

/// WCAG contrast ratio, 1..21.
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

fn to_hsl(c: Rgb) -> (f64, f64, f64) {
    let (r, g, b) = (
        f64::from(c.0) / 255.0,
        f64::from(c.1) / 255.0,
        f64::from(c.2) / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d == 0.0 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, s, l)
}

fn from_hsl(h: f64, s: f64, l: f64) -> Rgb {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 % 6 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let ch = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Rgb(ch(r), ch(g), ch(b))
}

/// `tint` with its lightness moved just far enough that both `text` and
/// `muted` reach WCAG AA on it: darker under light text, lighter under dark
/// text. `None` if no lightness works (the caller keeps the theme background).
pub fn readable(tint: Rgb, text: Rgb, muted: Rgb) -> Option<Rgb> {
    let ok = |bg: Rgb| contrast(text, bg).min(contrast(muted, bg)) >= AA;
    let (h, s, l) = to_hsl(tint);
    let darker = luminance(text) > 0.5;
    (0..=50).find_map(|step| {
        let delta = f64::from(step) * 0.02;
        let l = if darker { l - delta } else { l + delta };
        if !(0.0..=1.0).contains(&l) {
            return None;
        }
        let candidate = from_hsl(h, s, l);
        ok(candidate).then_some(candidate)
    })
}

use std::sync::atomic::Ordering;

use slate_core::theme;

use crate::backend::Backend;
use crate::bootstrap::AppWindow;
use crate::imgcache::Pixels;

impl Backend {
    /// The playing track's cover has been decoded. Its main colour is worked
    /// out on a blocking thread, and only if the setting is on.
    pub(crate) fn cover_arrived(&self, track_id: u64, px: Pixels) {
        if self.current_track_id() != Some(track_id) {
            return;
        }
        if let Ok(mut slot) = self.shared.cover_px.lock() {
            *slot = Some(px.clone());
        }
        if !self.shared.tint_enabled.load(Ordering::Relaxed) {
            return;
        }
        let this = self.clone();
        self.rt.spawn(async move {
            let found = tokio::task::spawn_blocking(move || dominant(&px))
                .await
                .ok()
                .flatten();
            // The track may have changed while the colour was being worked out.
            // Checked under the slot's lock, which `clear_tint` also takes, so
            // a skip between the check and the write cannot leave this colour.
            {
                let Ok(mut slot) = this.shared.cover_color.lock() else {
                    return;
                };
                if this.current_track_id() != Some(track_id) {
                    return;
                }
                *slot = found;
            }
            this.reapply_tint();
        });
    }

    /// No cover (or none that could be loaded) for `track_id`: back to the theme.
    pub(crate) fn clear_tint(&self, track_id: u64) {
        if self.current_track_id() != Some(track_id) {
            return;
        }
        if let Ok(mut slot) = self.shared.cover_px.lock() {
            *slot = None;
        }
        if let Ok(mut slot) = self.shared.cover_color.lock() {
            *slot = None;
        }
        self.reapply_tint();
    }

    /// Shows the tint made readable on the current theme, or the theme itself
    /// when there is no colour or the setting is off.
    pub(crate) fn reapply_tint(&self) {
        let enabled = self.shared.tint_enabled.load(Ordering::Relaxed);
        let raw = self.shared.cover_color.lock().ok().and_then(|c| *c);
        let t = self.current_theme_pub();
        let d = theme::derive(&t.accent, &t.bg_base);
        let shown = enabled
            .then_some(raw)
            .flatten()
            .zip(Rgb::from_hex(&d.text_primary).zip(Rgb::from_hex(&d.text_muted)))
            .and_then(|(tint, (text, muted))| readable(tint, text, muted));
        self.ui(move |w| apply_tint(&w, shown));
    }

    /// Settings → General toggle. Turning it on tints from the cover already
    /// on screen; turning it off restores the theme at once.
    pub(crate) fn set_cover_colors(&self, enabled: bool) -> Result<(), String> {
        let mut prefs = crate::uiprefs::UiPrefs::load();
        prefs.cover_colors = enabled;
        let saved = prefs.save().map_err(|e| e.to_string());
        self.shared.tint_enabled.store(enabled, Ordering::Relaxed);
        if enabled {
            let current = self.current_track_id();
            let px = self.shared.cover_px.lock().ok().and_then(|p| p.clone());
            if let (Some(id), Some(px)) = (current, px) {
                self.cover_arrived(id, px);
            }
        } else {
            self.reapply_tint();
        }
        saved
    }
}

fn apply_tint(w: &AppWindow, tint: Option<Rgb>) {
    match tint {
        Some(Rgb(r, g, b)) => {
            w.set_tint(slint::Color::from_rgb_u8(r, g, b));
            w.set_tint_on(true);
        }
        None => w.set_tint_on(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, c: [u8; 4]) -> SharedPixelBuffer<Rgba8Pixel> {
        let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
        for p in buf.make_mut_slice() {
            *p = Rgba8Pixel {
                r: c[0],
                g: c[1],
                b: c[2],
                a: c[3],
            };
        }
        buf
    }

    #[test]
    fn a_red_cover_gives_red() {
        let c = dominant(&solid(64, 64, [200, 30, 40, 255])).expect("colour");
        assert!(c.0 > 150 && c.1 < 80 && c.2 < 80, "{c:?}");
    }

    #[test]
    fn grey_black_white_and_transparent_covers_have_no_colour() {
        for c in [
            [128, 128, 128, 255],
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [200, 30, 40, 0],
        ] {
            assert_eq!(dominant(&solid(40, 40, c)), None, "{c:?}");
        }
        assert_eq!(dominant(&SharedPixelBuffer::<Rgba8Pixel>::new(0, 0)), None);
    }

    #[test]
    fn the_largest_colourful_area_wins_over_grey_and_small_accents() {
        let mut buf = solid(64, 64, [120, 120, 120, 255]);
        let w = 64usize;
        {
            let px = buf.make_mut_slice();
            for y in 0..64 {
                for x in 0..64 {
                    // Blue on the left 60 %, a little red top-right, the rest grey.
                    if x < 38 {
                        px[y * w + x] = Rgba8Pixel {
                            r: 20,
                            g: 60,
                            b: 220,
                            a: 255,
                        };
                    } else if x > 58 && y < 8 {
                        px[y * w + x] = Rgba8Pixel {
                            r: 230,
                            g: 20,
                            b: 20,
                            a: 255,
                        };
                    }
                }
            }
        }
        let c = dominant(&buf).expect("colour");
        assert!(c.2 > c.0 && c.2 > c.1, "{c:?}");
    }

    #[test]
    fn a_tiny_speck_of_colour_is_not_enough() {
        let mut buf = solid(100, 100, [120, 120, 120, 255]);
        buf.make_mut_slice()[0] = Rgba8Pixel {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        assert_eq!(dominant(&buf), None);
    }

    #[test]
    fn contrast_matches_the_wcag_reference_values() {
        assert!((contrast(Rgb(0, 0, 0), Rgb(255, 255, 255)) - 21.0).abs() < 0.01);
        assert!((contrast(Rgb(119, 119, 119), Rgb(255, 255, 255)) - 4.48).abs() < 0.05);
    }

    #[test]
    fn readable_tints_pass_aa_for_dark_and_light_themes() {
        let dark_text = (Rgb(255, 255, 255), Rgb(0xa6, 0xa6, 0xa6));
        let light_text = (Rgb(0x11, 0x11, 0x11), Rgb(0x66, 0x66, 0x66));
        for tint in [
            Rgb(200, 30, 40),
            Rgb(250, 230, 20),
            Rgb(20, 60, 220),
            Rgb(40, 200, 90),
            Rgb(255, 255, 255),
            Rgb(10, 10, 10),
        ] {
            for (text, muted) in [dark_text, light_text] {
                if let Some(bg) = readable(tint, text, muted) {
                    assert!(contrast(text, bg) >= AA, "{tint:?} {bg:?}");
                    assert!(contrast(muted, bg) >= AA, "{tint:?} {bg:?}");
                }
            }
        }
        // A saturated mid-tone must be adjustable under both.
        assert!(readable(Rgb(200, 30, 40), dark_text.0, dark_text.1).is_some());
        assert!(readable(Rgb(200, 30, 40), light_text.0, light_text.1).is_some());
    }

    #[test]
    fn already_readable_tints_are_left_alone() {
        let t = Rgb(30, 20, 60);
        assert_eq!(
            readable(t, Rgb(255, 255, 255), Rgb(0xa6, 0xa6, 0xa6)),
            Some(t)
        );
    }

    #[test]
    fn hex_colours_parse() {
        assert_eq!(Rgb::from_hex("#A855F7"), Some(Rgb(0xa8, 0x55, 0xf7)));
        assert_eq!(Rgb::from_hex("A855F7"), None);
        assert_eq!(Rgb::from_hex("#fff"), None);
    }
}
