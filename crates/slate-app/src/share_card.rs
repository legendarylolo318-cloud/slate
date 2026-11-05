//! "Copy share card": a 600×300 PNG with the cover, title, artist, album and the TIDAL link,
//! drawn once on demand and put on the clipboard. Nothing is kept: the font database, the
//! cover pixels and the PNG are dropped when the copy is done.
//!
//! The text is laid out as SVG and rasterised by resvg, which Slint already links for its own
//! SVG support, so shaping, right-to-left text and font fallback come for free. The cover
//! comes from the disk cache only (the thumbnail the track list shows); nothing is fetched.

use std::sync::Arc;

use resvg::{tiny_skia, usvg};
use slate_core::cache::{CacheResult, CacheTier};
use slate_core::queue::Track;
use slint::{Rgba8Pixel, SharedPixelBuffer};

use crate::backend::Backend;
use crate::textutil::clean_line;

pub const W: u32 = 600;
pub const H: u32 = 300;
const PAD: f32 = 30.0;
const COVER: f32 = 240.0;
const TEXT_X: f32 = 300.0;
const TEXT_W: f32 = W as f32 - TEXT_X - PAD;
const FONTS: &str = "Inter, Segoe UI, Noto Sans, DejaVu Sans, Arial, sans-serif";

/// Where a track lives on tidal.com.
pub fn track_link(id: u64) -> String {
    format!("https://tidal.com/browse/track/{id}")
}

/// What the card says.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub id: u64,
    pub title: String,
    pub artist: String,
    pub album: String,
}

impl Card {
    pub fn of(t: &Track) -> Self {
        let album = t
            .extra
            .get("album")
            .and_then(|a| a.get("title"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        Self {
            id: t.id,
            title: clean_line(t.title()),
            artist: clean_line(t.artist_name()),
            album: clean_line(album),
        }
    }
}

/// One line of text as placed on the card.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub text: String,
    pub size: f32,
    pub rtl: bool,
}

/// Widest `text` is allowed to be, in pixels, at `size` and weight: supplied by the caller so the
/// layout can be tested without any font.
pub type Measure<'a> = &'a dyn Fn(&str, f32, bool) -> f32;

/// First strong character decides the direction (Hebrew, Arabic, Syriac, Thaana, N'Ko, and
/// the presentation forms).
pub fn is_rtl(s: &str) -> bool {
    s.chars().find(|c| c.is_alphabetic()).is_some_and(|c| {
        matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF)
    })
}

/// Shrinks `text` from `start` towards `min` pixels until it fits `max_w`; at the smallest size
/// it is cut and ends in "…". Never panics and never returns something wider than `max_w`
/// unless even "…" does not fit.
pub fn fit(text: &str, max_w: f32, start: f32, min: f32, bold: bool, measure: Measure) -> Line {
    let rtl = is_rtl(text);
    let mut size = start;
    while size > min && measure(text, size, bold) > max_w {
        size -= 2.0;
    }
    let size = size.max(min);
    if measure(text, size, bold) <= max_w {
        return Line {
            text: text.to_string(),
            size,
            rtl,
        };
    }
    let chars: Vec<char> = text.chars().collect();
    // Longest prefix that fits with the ellipsis (binary search over the char count).
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let cut: String = chars[..mid]
            .iter()
            .collect::<String>()
            .trim_end()
            .to_string()
            + "…";
        if measure(&cut, size, bold) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let kept: String = chars[..lo].iter().collect();
    Line {
        text: kept.trim_end().to_string() + "…",
        size,
        rtl,
    }
}

/// The four lines of the card.
pub struct Layout {
    pub title: Line,
    pub artist: Line,
    pub album: Line,
    pub link: Line,
}

pub fn layout(card: &Card, measure: Measure) -> Layout {
    let or = |s: &str, d: &str| {
        if s.is_empty() {
            d.to_string()
        } else {
            s.to_string()
        }
    };
    Layout {
        title: fit(
            &or(&card.title, "Unknown title"),
            TEXT_W,
            30.0,
            18.0,
            true,
            measure,
        ),
        artist: fit(
            &or(&card.artist, "Unknown artist"),
            TEXT_W,
            20.0,
            14.0,
            false,
            measure,
        ),
        album: fit(&card.album, TEXT_W, 16.0, 12.0, false, measure),
        link: fit(&track_link(card.id), TEXT_W, 13.0, 9.0, false, measure),
    }
}

fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            c => o.push(c),
        }
    }
    o
}

fn text_el(l: &Line, y: f32, weight: u32, fill: &str) -> String {
    if l.text.is_empty() {
        return String::new();
    }
    let (x, anchor, dir) = if l.rtl {
        (TEXT_X + TEXT_W, "end", "rtl")
    } else {
        (TEXT_X, "start", "ltr")
    };
    format!(
        r#"<text x="{x}" y="{y}" font-family="{FONTS}" font-size="{}" font-weight="{weight}" fill="{fill}" text-anchor="{anchor}" direction="{dir}">{}</text>"#,
        l.size,
        escape(&l.text)
    )
}

/// The card as SVG (the cover is drawn separately).
pub fn svg(l: &Layout) -> String {
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#1f1f26"/><stop offset="1" stop-color="#0d0d10"/></linearGradient></defs><rect width="{W}" height="{H}" fill="url(#g)"/><rect x="{PAD}" y="{PAD}" width="{COVER}" height="{COVER}" rx="8" fill="#2a2a32"/>{}{}{}{}</svg>"##,
        text_el(&l.title, 112.0, 700, "#ffffff"),
        text_el(&l.artist, 150.0, 400, "#d0d0d8"),
        text_el(&l.album, 180.0, 400, "#9a9aa6"),
        text_el(&l.link, 262.0, 400, "#5fd1ff"),
    )
}

/// Measures with a real font database by parsing a one-line SVG.
fn measurer<'a>(opt: &'a usvg::Options<'a>) -> impl Fn(&str, f32, bool) -> f32 + 'a {
    move |text, size, bold| {
        let s = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="4000" height="200"><text x="0" y="100" font-family="{FONTS}" font-size="{size}" font-weight="{}">{}</text></svg>"#,
            if bold { 700 } else { 400 },
            escape(text)
        );
        usvg::Tree::from_str(&s, opt).map_or(f32::MAX, |t| t.root().abs_bounding_box().width())
    }
}

fn options() -> usvg::Options<'static> {
    let mut db = usvg::fontdb::Database::new();
    db.load_system_fonts();
    usvg::Options {
        fontdb: Arc::new(db),
        ..usvg::Options::default()
    }
}

/// A cover as a tiny-skia pixmap (premultiplied), or `None` for an empty buffer.
fn cover_pixmap(px: &SharedPixelBuffer<Rgba8Pixel>) -> Option<tiny_skia::Pixmap> {
    let mut map = tiny_skia::Pixmap::new(px.width(), px.height())?;
    for (dst, src) in map.pixels_mut().iter_mut().zip(px.as_slice()) {
        *dst = tiny_skia::ColorU8::from_rgba(src.r, src.g, src.b, src.a).premultiply();
    }
    Some(map)
}

/// Draws the card. `cover` is scaled to the cover square; without one the square stays a plain
/// rounded rectangle. Returns straight RGBA (the card is opaque).
pub fn render(card: &Card, cover: Option<&SharedPixelBuffer<Rgba8Pixel>>) -> Option<Vec<u8>> {
    let opt = options();
    let measure = measurer(&opt);
    let lay = layout(card, &measure);
    let tree = usvg::Tree::from_str(&svg(&lay), &opt).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(W, H)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    if let Some(cover) = cover.and_then(cover_pixmap) {
        let mut rr = tiny_skia::PathBuilder::new();
        let (x, y, w, r) = (PAD, PAD, COVER, 8.0f32);
        rr.move_to(x + r, y);
        rr.line_to(x + w - r, y);
        rr.quad_to(x + w, y, x + w, y + r);
        rr.line_to(x + w, y + w - r);
        rr.quad_to(x + w, y + w, x + w - r, y + w);
        rr.line_to(x + r, y + w);
        rr.quad_to(x, y + w, x, y + w - r);
        rr.line_to(x, y + r);
        rr.quad_to(x, y, x + r, y);
        rr.close();
        let path = rr.finish()?;
        let (sx, sy) = (COVER / cover.width() as f32, COVER / cover.height() as f32);
        let paint = tiny_skia::Paint {
            shader: tiny_skia::Pattern::new(
                cover.as_ref(),
                tiny_skia::SpreadMode::Pad,
                tiny_skia::FilterQuality::Bicubic,
                1.0,
                tiny_skia::Transform::from_row(sx, 0.0, 0.0, sy, PAD, PAD),
            ),
            anti_alias: true,
            ..tiny_skia::Paint::default()
        };
        pixmap.fill_path(
            &path,
            &paint,
            tiny_skia::FillRule::Winding,
            tiny_skia::Transform::identity(),
            None,
        );
    }
    // Opaque everywhere, so premultiplied and straight alpha are the same bytes.
    Some(pixmap.take())
}

/// What happened to the finished card.
#[derive(Debug)]
pub enum Delivered {
    Clipboard,
    Saved(std::path::PathBuf),
    Failed(String),
}

/// Puts the card on the clipboard; when the clipboard cannot hold an image, saves a PNG in the
/// Pictures folder instead.
fn deliver(id: u64, rgba: Vec<u8>) -> Delivered {
    let set = arboard::Clipboard::new().and_then(|mut c| {
        c.set_image(arboard::ImageData {
            width: W as usize,
            height: H as usize,
            bytes: std::borrow::Cow::Borrowed(&rgba),
        })
    });
    if set.is_ok() {
        return Delivered::Clipboard;
    }
    let Some(dir) = dirs::picture_dir().or_else(dirs::home_dir) else {
        return Delivered::Failed("no Pictures folder".into());
    };
    let path = dir.join(format!("slate-share-{id}.png"));
    let Some(img) = image::RgbaImage::from_raw(W, H, rgba) else {
        return Delivered::Failed("bad image".into());
    };
    match std::fs::create_dir_all(&dir)
        .and_then(|()| img.save(&path).map_err(std::io::Error::other))
    {
        Ok(()) => Delivered::Saved(path),
        Err(e) => Delivered::Failed(e.to_string()),
    }
}

impl Backend {
    /// The cached cover of `track`, if the disk cache has one: the 320 px thumbnail of the
    /// player bar, else the 160 px one of the track lists. Never goes to the network.
    async fn cached_cover(&self, track: &Track) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
        let state = self.state()?;
        let id = track.cover_id()?;
        for size in [320, 160] {
            let url = crate::images::tidal_image_url(id, size);
            if let CacheResult::Fresh(b) | CacheResult::Stale(b) =
                state.disk_cache.get(&url, CacheTier::Image).await
            {
                if let Some(px) =
                    tokio::task::spawn_blocking(move || crate::images::decode_fit(&b, 0))
                        .await
                        .ok()
                        .flatten()
                {
                    return Some(px);
                }
            }
        }
        None
    }

    /// "Copy share card".
    pub(crate) async fn copy_share_card(&self, track: &Track) {
        let card = Card::of(track);
        let cover = self.cached_cover(track).await;
        let id = card.id;
        let done = tokio::task::spawn_blocking(move || {
            let rgba = render(&card, cover.as_ref())?;
            drop(cover);
            Some(deliver(id, rgba))
        })
        .await
        .ok()
        .flatten();
        match done {
            Some(Delivered::Clipboard) => self.notify("Share card copied"),
            Some(Delivered::Saved(p)) => self.notify(&format!(
                "Image clipboard unavailable; saved the card to {}",
                p.display()
            )),
            Some(Delivered::Failed(e)) => {
                self.ui(move |w| w.set_error(format!("Could not make the share card: {e}").into()));
            }
            None => self.ui(|w| w.set_error("Could not make the share card.".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(text: &str, size: f32, _bold: bool) -> f32 {
        text.chars().count() as f32 * size * 0.55
    }

    fn card(title: &str, artist: &str, album: &str) -> Card {
        Card {
            id: 42,
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
        }
    }

    #[test]
    fn link_is_the_browse_url() {
        assert_eq!(track_link(77), "https://tidal.com/browse/track/77");
    }

    #[test]
    fn short_text_keeps_full_size() {
        let l = fit("Hi", 270.0, 30.0, 18.0, true, &fake);
        assert_eq!((l.text.as_str(), l.size), ("Hi", 30.0));
    }

    #[test]
    fn long_text_shrinks_then_is_cut_with_an_ellipsis() {
        let long = "x".repeat(500);
        let l = fit(&long, 270.0, 30.0, 18.0, true, &fake);
        assert_eq!(l.size, 18.0);
        assert!(l.text.ends_with('…'));
        assert!(fake(&l.text, l.size, true) <= 270.0);
        assert!(l.text.chars().count() > 1);
    }

    #[test]
    fn rtl_and_mixed_scripts_do_not_panic() {
        for t in [
            "שלום עולם ".repeat(40),
            "مرحبا بالعالم ".repeat(40),
            "日本語のとても長いタイトル".repeat(30),
            "👩‍👩‍👧‍👦 emoji ".repeat(40),
            String::new(),
            "\u{202E}evil".into(),
        ] {
            let c = card(&clean_line(&t), &clean_line(&t), &clean_line(&t));
            let lay = layout(&c, &fake);
            let s = svg(&lay);
            assert!(s.starts_with("<svg"));
        }
        assert!(is_rtl("שלום"));
        assert!(is_rtl("123 مرحبا"));
        assert!(!is_rtl("Hello שלום"));
    }

    #[test]
    fn markup_in_titles_is_escaped() {
        let lay = layout(&card("</text><script>&", "a\"b", ""), &fake);
        let s = svg(&lay);
        assert!(!s.contains("<script>"));
        assert!(s.contains("&lt;/text&gt;"));
    }

    #[test]
    fn renders_a_png_sized_card_with_and_without_cover() {
        let c = card(&"Very long title ".repeat(20), "שלום עולם", "Album");
        let plain = render(&c, None).expect("card");
        assert_eq!(plain.len(), (W * H * 4) as usize);
        let mut cover = SharedPixelBuffer::<Rgba8Pixel>::new(160, 160);
        for p in cover.make_mut_slice() {
            *p = Rgba8Pixel {
                r: 200,
                g: 30,
                b: 30,
                a: 255,
            };
        }
        let with = render(&c, Some(&cover)).expect("card");
        assert_ne!(plain, with);
        // The cover square shows the cover colour.
        let at = ((60 * W + 60) * 4) as usize;
        assert_eq!(&with[at..at + 3], &[200, 30, 30]);
    }

    /// On demand: `SLATE_CARD_OUT=/tmp/card.png cargo test -p slate --release write_sample -- --ignored`.
    #[test]
    #[ignore]
    fn write_sample() {
        let Ok(out) = std::env::var("SLATE_CARD_OUT") else {
            return;
        };
        let c = card(
            "Nights in White Satin (Extended Anniversary Remaster)",
            "The Moody Blues",
            "Days of Future Passed",
        );
        let mut cover = SharedPixelBuffer::<Rgba8Pixel>::new(160, 160);
        for (i, p) in cover.make_mut_slice().iter_mut().enumerate() {
            *p = Rgba8Pixel {
                r: (i % 160) as u8,
                g: (i / 160) as u8,
                b: 120,
                a: 255,
            };
        }
        let rgba = render(&c, Some(&cover)).expect("card");
        image::RgbaImage::from_raw(W, H, rgba)
            .expect("size")
            .save(out)
            .expect("save");
    }
}
