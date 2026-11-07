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
use crate::cover_tint::{contrast, dominant, Rgb};
use crate::textutil::clean_line;

pub const W: u32 = 600;
pub const H: u32 = 300;
const PAD: f32 = 30.0;
const COVER: f32 = 240.0;
const TEXT_X: f32 = 300.0;
const TEXT_W: f32 = W as f32 - TEXT_X - PAD;
const FONTS: &str = "Inter, Segoe UI, Noto Sans, DejaVu Sans, Arial, sans-serif";
/// Emoji runs get their own span in a colour emoji font (the text font has none, and a
/// joined emoji such as 👩‍👩‍👧‍👦 only shapes as one glyph in a font that has it).
const EMOJI_FONTS: &str =
    "Noto Color Emoji, Apple Color Emoji, Segoe UI Emoji, Twemoji Mozilla, Segoe UI Symbol";
/// Where a character the text font lacks is looked up first, in this order. Without this
/// usvg takes the first face on the system that has the character, which on a machine with
/// programming fonts was a monospaced one, and re-shapes the whole line in it (seen: Hebrew
/// and mixed lines in Cascadia Code, Arabic letters left unjoined, emoji as boxes).
const FALLBACK: &[&str] = &[
    "Noto Sans",
    "Noto Sans Arabic",
    "Noto Naskh Arabic",
    "Noto Sans Hebrew",
    "Noto Sans CJK JP",
    "Noto Sans CJK KR",
    "Noto Sans CJK SC",
    "Noto Sans CJK TC",
    "Noto Sans JP",
    "Noto Sans KR",
    "Noto Sans SC",
    "Source Han Sans",
    "Segoe UI",
    "Microsoft YaHei UI",
    "Yu Gothic UI",
    "Malgun Gothic",
    "Nirmala UI",
    "Leelawadee UI",
    "Tahoma",
    "DejaVu Sans",
    "Droid Sans Fallback",
    "Arial",
    "Noto Color Emoji",
    "Segoe UI Emoji",
    "Segoe UI Symbol",
    "Noto Sans Symbols",
    "Noto Sans Symbols 2",
];

/// Where a track lives on tidal.com.
pub fn track_link(id: u64) -> String {
    format!("https://tidal.com/browse/track/{id}")
}

/// The look of a card: `Dark` is the original.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    Dark,
    Light,
    /// Tinted with the cover's main colour (plain dark when the cover has none).
    Album,
    /// A flat page with the title, artist and link only.
    Minimal,
}

impl Theme {
    #[cfg(test)]
    pub const ALL: [Self; 4] = [Self::Dark, Self::Light, Self::Album, Self::Minimal];

    pub fn parse(s: &str) -> Self {
        match s {
            "light" => Self::Light,
            "album" => Self::Album,
            "minimal" => Self::Minimal,
            _ => Self::Dark,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::Album => "album",
            Self::Minimal => "minimal",
        }
    }
}

/// Colours (as `#rrggbb`) and which lines a theme shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    /// Background at the top left and bottom right (equal: a flat page).
    pub bg: (String, String),
    pub cover_bg: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub link: String,
    pub show_album: bool,
}

fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let m = |x: u8, y: u8| (f64::from(x) * (1.0 - t) + f64::from(y) * t).round() as u8;
    Rgb(m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// `fg`, moved towards `toward` in tenths until it reads on `bg` (WCAG AA for text).
fn readable_on(fg: Rgb, bg: Rgb, toward: Rgb) -> Rgb {
    let mut c = fg;
    for step in 1..=10 {
        if contrast(c, bg) >= 4.5 {
            break;
        }
        c = mix(fg, toward, f64::from(step) / 10.0);
    }
    c
}

pub fn palette(theme: Theme, accent: Option<Rgb>) -> Palette {
    let flat = |c: &str| (c.to_string(), c.to_string());
    match theme {
        Theme::Dark => Palette {
            bg: ("#1f1f26".into(), "#0d0d10".into()),
            cover_bg: "#2a2a32".into(),
            title: "#ffffff".into(),
            artist: "#d0d0d8".into(),
            album: "#9a9aa6".into(),
            link: "#5fd1ff".into(),
            show_album: true,
        },
        Theme::Light => Palette {
            bg: ("#f7f7f9".into(), "#e4e4ea".into()),
            cover_bg: "#d3d3da".into(),
            title: "#111114".into(),
            artist: "#33333b".into(),
            album: "#55555f".into(),
            link: "#075a93".into(),
            show_album: true,
        },
        Theme::Minimal => Palette {
            bg: flat("#fafafa"),
            cover_bg: "#e8e8ec".into(),
            title: "#111114".into(),
            artist: "#4a4a52".into(),
            album: "#4a4a52".into(),
            link: "#5f5f68".into(),
            show_album: false,
        },
        Theme::Album => {
            let Some(dom) = accent else {
                return palette(Theme::Dark, None);
            };
            let black = Rgb(0, 0, 0);
            let white = Rgb(255, 255, 255);
            let (top, bottom) = (mix(dom, black, 0.55), mix(dom, black, 0.82));
            // Every text colour must read on the lighter end of the background.
            let on = |c: Rgb| readable_on(c, top, white);
            Palette {
                bg: (hex(top), hex(bottom)),
                cover_bg: hex(mix(top, white, 0.12)),
                title: hex(white),
                artist: hex(on(mix(dom, white, 0.82))),
                album: hex(on(mix(dom, white, 0.65))),
                link: hex(on(mix(dom, white, 0.55))),
                show_album: true,
            }
        }
    }
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
        let album = t.album_title();
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

/// Starts or continues an emoji: pictographs, symbols drawn as emoji, flags, keycaps, and the
/// joiners, selectors, skin tones and tags inside a sequence.
/// (Flags, U+1F1E6–1F1FF, are inside the first range.)
fn is_emoji(c: char, in_emoji: bool) -> bool {
    let u = c as u32;
    matches!(u, 0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF)
        || (in_emoji && matches!(u, 0x200D | 0xFE0F | 0x20E3 | 0xE0020..=0xE007F))
}

/// What a run of text is shaped as: one script, emoji, or the closing ellipsis.
#[derive(Clone, Copy, PartialEq)]
enum Run {
    Text(Option<[u8; 4]>),
    Emoji,
    Ellipsis,
}

/// `text` as escaped SVG content, cut into `<tspan>`s so that each is shaped with one font:
/// emoji in an emoji font, each script on its own, and the "…" that ends a cut line apart.
/// usvg shapes a span with its first font and patches missing glyphs from a fallback only
/// when both give the same number of glyphs; Arabic ligatures (or emoji sequences) next to a
/// character the fallback lacks broke that, and the whole line came out as boxes.
fn spans(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 32);
    let mut run = String::new();
    let mut kind = Run::Text(None);
    let flush = |out: &mut String, run: &mut String, kind: Run| {
        if run.is_empty() {
            return;
        }
        match kind {
            Run::Emoji => out.push_str(&format!(
                r#"<tspan font-family="{EMOJI_FONTS}">{}</tspan>"#,
                escape(run)
            )),
            _ => out.push_str(&format!("<tspan>{}</tspan>", escape(run))),
        }
        run.clear();
    };
    for c in text.chars() {
        let next = if c == '…' {
            Run::Ellipsis
        } else if is_emoji(c, kind == Run::Emoji) {
            Run::Emoji
        } else {
            match (script_of(c), kind) {
                // Spaces, digits and punctuation stay with the run they are in.
                (None, Run::Text(k)) => Run::Text(k),
                (s, _) => Run::Text(s),
            }
        };
        let same = match (next, kind) {
            (Run::Text(None), Run::Text(_)) | (Run::Text(_), Run::Text(None)) => true,
            (a, b) => a == b,
        };
        if !same {
            flush(&mut out, &mut run, kind);
        }
        if !same || matches!(next, Run::Text(Some(_))) {
            kind = next;
        }
        run.push(c);
    }
    flush(&mut out, &mut run, kind);
    out
}

fn has_char(db: &usvg::fontdb::Database, id: usvg::fontdb::ID, c: char) -> bool {
    use skrifa::MetadataProvider;
    db.with_face_data(id, |data, index| {
        skrifa::FontRef::from_index(data, index)
            .ok()
            .and_then(|f| f.charmap().map(c))
            .is_some()
    })
    .unwrap_or(false)
}

/// The face for a character the line's font lacks (see [`FALLBACK`]): the first preferred
/// family that has it, in the line's weight if there is one; then any proportional face; then
/// any face at all.
fn fallback(
    c: char,
    used: &[usvg::fontdb::ID],
    db: &mut Arc<usvg::fontdb::Database>,
) -> Option<usvg::fontdb::ID> {
    let weight = used.first().and_then(|id| db.face(*id)).map(|f| f.weight);
    let usable = |f: &usvg::fontdb::FaceInfo| !used.contains(&f.id) && has_char(db, f.id, c);
    for name in FALLBACK {
        let mut any = None;
        for f in db
            .faces()
            .filter(|f| f.families.iter().any(|(n, _)| n == name))
        {
            if f.style != usvg::fontdb::Style::Normal || !usable(f) {
                continue;
            }
            if Some(f.weight) == weight {
                return Some(f.id);
            }
            any.get_or_insert(f.id);
        }
        if any.is_some() {
            return any;
        }
    }
    db.faces()
        .find(|f| !f.monospaced && usable(f))
        .or_else(|| db.faces().find(|f| usable(f)))
        .map(|f| f.id)
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
        spans(&l.text)
    )
}

/// The card as SVG (the cover is drawn separately).
pub fn svg(l: &Layout, p: &Palette) -> String {
    let album = if p.show_album {
        text_el(&l.album, 180.0, 400, &p.album)
    } else {
        String::new()
    };
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="{}"/><stop offset="1" stop-color="{}"/></linearGradient></defs><rect width="{W}" height="{H}" fill="url(#g)"/><rect x="{PAD}" y="{PAD}" width="{COVER}" height="{COVER}" rx="8" fill="{}"/>{}{}{}{}</svg>"##,
        p.bg.0,
        p.bg.1,
        p.cover_bg,
        text_el(&l.title, 112.0, 700, &p.title),
        text_el(&l.artist, 150.0, 400, &p.artist),
        album,
        text_el(&l.link, 262.0, 400, &p.link),
    )
}

/// Measures with a real font database by parsing a one-line SVG.
fn measurer<'a>(opt: &'a usvg::Options<'a>) -> impl Fn(&str, f32, bool) -> f32 + 'a {
    move |text, size, bold| {
        let s = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="4000" height="200"><text x="0" y="100" font-family="{FONTS}" font-size="{size}" font-weight="{}">{}</text></svg>"#,
            if bold { 700 } else { 400 },
            spans(text)
        );
        usvg::Tree::from_str(&s, opt).map_or(f32::MAX, |t| t.root().abs_bounding_box().width())
    }
}

/// The script fontique keys system fallbacks on, for the scripts a title is likely to use.
fn script_of(c: char) -> Option<[u8; 4]> {
    Some(match c as u32 {
        0x0370..=0x03FF => *b"Grek",
        0x0400..=0x052F => *b"Cyrl",
        0x0530..=0x058F => *b"Armn",
        0x0590..=0x05FF | 0xFB1D..=0xFB4F => *b"Hebr",
        0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF => {
            *b"Arab"
        }
        0x0900..=0x097F => *b"Deva",
        0x0980..=0x09FF => *b"Beng",
        0x0B80..=0x0BFF => *b"Taml",
        0x0E00..=0x0E7F => *b"Thai",
        0x10A0..=0x10FF => *b"Geor",
        0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF => *b"Hang",
        0x3040..=0x309F => *b"Hira",
        0x30A0..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F => *b"Kana",
        0x2E80..=0x2FDF
        | 0x3000..=0x303F
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xF900..=0xFAFF
        | 0xFF00..=0xFF65
        | 0x20000..=0x3FFFF => *b"Hani",
        _ => return None,
    })
}

/// Family names in a CSS-style list.
fn names(list: &str) -> impl Iterator<Item = &str> {
    list.split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty() && *n != "sans-serif")
}

/// A font database holding only what `texts` can need: the card's own fonts, the emoji fonts,
/// the system's fallback fonts for every script in the texts, and [`FALLBACK`]. The files are
/// found through the system font index (fontconfig, DirectWrite) and memory-mapped, so a
/// machine with thousands of fonts does not open every one (loading all system fonts took
/// 12 s here, with 10,007 faces in 14 GB).
fn font_db(texts: &[&str]) -> usvg::fontdb::Database {
    let mut coll = fontique::Collection::new(fontique::CollectionOptions {
        shared: false,
        system_fonts: true,
    });
    let mut families: Vec<fontique::FamilyId> = Vec::new();
    for name in names(FONTS)
        .chain(names(EMOJI_FONTS))
        .chain(FALLBACK.iter().copied())
    {
        if let Some(id) = coll.family_id(name) {
            families.push(id);
        }
    }
    let mut scripts: Vec<[u8; 4]> = texts
        .iter()
        .flat_map(|t| t.chars())
        .filter_map(script_of)
        .collect();
    scripts.sort_unstable();
    scripts.dedup();
    for sc in scripts {
        let key = fontique::FallbackKey::from(fontique::Script::from_bytes(sc));
        families.extend(coll.fallback_families(key).take(2));
    }
    let mut db = usvg::fontdb::Database::new();
    let mut seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    for id in families {
        let Some(fam) = coll.family(id) else { continue };
        for font in fam.fonts() {
            match &font.source().kind {
                fontique::SourceKind::Path(p) => {
                    if seen.insert(p.to_path_buf()) {
                        let _ = db.load_font_file(p);
                    }
                }
                fontique::SourceKind::Memory(blob) => db.load_font_data(blob.as_ref().to_vec()),
            }
        }
    }
    if let Some(first) =
        names(FONTS).find(|n| db.faces().any(|f| f.families.iter().any(|(m, _)| m == n)))
    {
        db.set_sans_serif_family(first);
    }
    db
}

fn options(texts: &[&str]) -> usvg::Options<'static> {
    usvg::Options {
        fontdb: Arc::new(font_db(texts)),
        font_resolver: usvg::FontResolver {
            select_font: usvg::FontResolver::default_font_selector(),
            select_fallback: Box::new(fallback),
        },
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
pub fn render(
    card: &Card,
    cover: Option<&SharedPixelBuffer<Rgba8Pixel>>,
    theme: Theme,
) -> Option<Vec<u8>> {
    let opt = options(&[&card.title, &card.artist, &card.album]);
    let measure = measurer(&opt);
    let lay = layout(card, &measure);
    let accent = (theme == Theme::Album)
        .then(|| cover.and_then(dominant))
        .flatten();
    let tree = usvg::Tree::from_str(&svg(&lay, &palette(theme, accent)), &opt).ok()?;
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
    let set = crate::clipboard::set_image(W as usize, H as usize, &rgba);
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
    /// `theme`: the one just picked in the menu (and remembered); `None` is the last choice.
    pub(crate) async fn copy_share_card(&self, track: &Track, theme: Option<Theme>) {
        let theme = match theme {
            Some(t) => {
                let mut prefs = crate::uiprefs::UiPrefs::load();
                if prefs.share_theme != t.key() {
                    prefs.share_theme = t.key().to_string();
                    let _ = prefs.save();
                }
                t
            }
            None => Theme::parse(&crate::uiprefs::UiPrefs::load().share_theme),
        };
        let card = Card::of(track);
        let cover = self.cached_cover(track).await;
        let id = card.id;
        let done = tokio::task::spawn_blocking(move || {
            let rgba = render(&card, cover.as_ref(), theme)?;
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
            let s = svg(&lay, &palette(Theme::Dark, None));
            assert!(s.starts_with("<svg"));
        }
        assert!(is_rtl("שלום"));
        assert!(is_rtl("123 مرحبا"));
        assert!(!is_rtl("Hello שלום"));
    }

    #[test]
    fn markup_in_titles_is_escaped() {
        let lay = layout(&card("</text><script>&", "a\"b", ""), &fake);
        let s = svg(&lay, &palette(Theme::Dark, None));
        assert!(!s.contains("<script>"));
        assert!(s.contains("&lt;/text&gt;"));
    }

    #[test]
    fn renders_a_png_sized_card_with_and_without_cover() {
        let c = card(&"Very long title ".repeat(20), "שלום עולם", "Album");
        let plain = render(&c, None, Theme::Dark).expect("card");
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
        let with = render(&c, Some(&cover), Theme::Dark).expect("card");
        assert_ne!(plain, with);
        // The cover square shows the cover colour.
        let at = ((60 * W + 60) * 4) as usize;
        assert_eq!(&with[at..at + 3], &[200, 30, 30]);
    }

    #[test]
    fn every_theme_reads_on_its_background() {
        let accents = [
            None,
            Some(Rgb(255, 255, 0)),
            Some(Rgb(255, 255, 255)),
            Some(Rgb(0, 0, 255)),
            Some(Rgb(230, 30, 30)),
            Some(Rgb(30, 200, 90)),
            Some(Rgb(120, 60, 200)),
            Some(Rgb(250, 250, 210)),
        ];
        for theme in Theme::ALL {
            for a in accents {
                let p = palette(theme, a);
                let hexc = |h: &str| Rgb::from_hex(h).expect("hex");
                for bg in [hexc(&p.bg.0), hexc(&p.bg.1)] {
                    let mut texts = vec![&p.title, &p.artist, &p.link];
                    if p.show_album {
                        texts.push(&p.album);
                    }
                    for t in texts {
                        let c = contrast(hexc(t), bg);
                        assert!(c >= 4.5, "{theme:?} {a:?}: {t} on {bg:?} is {c:.2}");
                    }
                }
            }
        }
    }

    #[test]
    fn themes_have_keys_that_parse_back_and_the_default_is_dark() {
        for t in Theme::ALL {
            assert_eq!(Theme::parse(t.key()), t);
        }
        assert_eq!(Theme::parse(""), Theme::Dark);
        assert_eq!(Theme::parse("garbage"), Theme::Dark);
        assert_eq!(Theme::default(), Theme::Dark);
    }

    #[test]
    fn the_album_theme_follows_the_cover_and_falls_back_to_dark() {
        assert_eq!(palette(Theme::Album, None), palette(Theme::Dark, None));
        let red = palette(Theme::Album, Some(Rgb(230, 30, 30)));
        let blue = palette(Theme::Album, Some(Rgb(30, 30, 230)));
        assert_ne!(red.bg, blue.bg);
        assert_ne!(red.bg, palette(Theme::Dark, None).bg);
    }

    #[test]
    fn minimal_hides_the_album_line_and_the_others_keep_it() {
        let lay = layout(&card("T", "A", "Some Album"), &fake);
        let minimal = svg(&lay, &palette(Theme::Minimal, None));
        let dark = svg(&lay, &palette(Theme::Dark, None));
        assert!(!minimal.contains("Some Album") && dark.contains("Some Album"));
        assert!(svg(&lay, &palette(Theme::Light, None)).contains("Some Album"));
    }

    #[test]
    fn every_theme_renders_the_same_sized_png_and_the_themes_differ() {
        let c = card("Title", "Artist", "Album");
        let mut cover = SharedPixelBuffer::<Rgba8Pixel>::new(64, 64);
        for p in cover.make_mut_slice() {
            *p = Rgba8Pixel {
                r: 220,
                g: 40,
                b: 40,
                a: 255,
            };
        }
        let cards: Vec<Vec<u8>> = Theme::ALL
            .iter()
            .map(|t| render(&c, Some(&cover), *t).expect("card"))
            .collect();
        for card in &cards {
            assert_eq!(card.len(), (W * H * 4) as usize);
        }
        for i in 0..cards.len() {
            for j in i + 1..cards.len() {
                assert_ne!(
                    cards[i],
                    cards[j],
                    "{:?} vs {:?}",
                    Theme::ALL[i],
                    Theme::ALL[j]
                );
            }
        }
        // The corner shows each theme's own background; Light is light, Dark is dark.
        let px = |c: &Vec<u8>| c[0] as u32 + c[1] as u32 + c[2] as u32;
        assert!(px(&cards[1]) > 600 && px(&cards[0]) < 150);
    }

    /// On demand: `SLATE_CARD_DIR=/tmp/cards cargo test -p slate --release write_theme_samples -- --ignored`
    /// writes one card per theme.
    #[test]
    #[ignore]
    fn write_theme_samples() {
        let Ok(dir) = std::env::var("SLATE_CARD_DIR") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        let mut cover = SharedPixelBuffer::<Rgba8Pixel>::new(160, 160);
        for (i, p) in cover.make_mut_slice().iter_mut().enumerate() {
            *p = Rgba8Pixel {
                r: 40 + (i % 160 / 3) as u8,
                g: 60,
                b: 170 - (i / 160 / 2) as u8,
                a: 255,
            };
        }
        let c = card("Africa", "TOTO", "Toto IV");
        for t in Theme::ALL {
            let rgba = render(&c, Some(&cover), t).expect("card");
            image::RgbaImage::from_raw(W, H, rgba)
                .expect("size")
                .save(dir.join(format!("theme-{}.png", t.key())))
                .expect("save");
        }
    }

    /// On demand: `SLATE_CARD_DIR=/tmp/cards cargo test -p slate --release write_samples -- --ignored`
    /// writes one card per hard case (long, CJK, emoji, right-to-left, mixed, no cover).
    #[test]
    #[ignore]
    fn write_samples() {
        let Ok(dir) = std::env::var("SLATE_CARD_DIR") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        let mut cover = SharedPixelBuffer::<Rgba8Pixel>::new(160, 160);
        for (i, p) in cover.make_mut_slice().iter_mut().enumerate() {
            *p = Rgba8Pixel {
                r: (i % 160) as u8,
                g: (i / 160) as u8,
                b: 120,
                a: 255,
            };
        }
        let cases: [(&str, Card, bool); 7] = [
            ("long", card("Nights in White Satin (Extended Anniversary Remaster) [Live at the Royal Albert Hall, 1969]", "The Moody Blues & The London Festival Orchestra", "Days of Future Passed (Deluxe Edition)"), true),
            ("cjk", card("残酷な天使のテーゼ (Director's Edit Version)", "高橋洋子", "新世紀エヴァンゲリオン"), true),
            ("korean", card("봄날 (Spring Day)", "방탄소년단", "YOU NEVER WALK ALONE"), true),
            ("emoji", card("🔥 Fire 🔥 👩‍👩‍👧‍👦", "Artist 🎧", "Album ✨"), true),
            ("rtl-hebrew", card("שיר לשלום", "להקת הנח\"ל", "שירים"), true),
            ("rtl-arabic", card("أنا لحبيبي وحبيبي إلي يا عصفورة بيضا", "فيروز", "أنا لحبيبي"), true),
            ("no-cover", card(&clean_line("Mixed English עברית 123 \u{202E}override"), "Somebody", ""), false),
        ];
        for (name, c, with_cover) in cases {
            let rgba = render(&c, with_cover.then_some(&cover), Theme::Dark).expect("card");
            image::RgbaImage::from_raw(W, H, rgba)
                .expect("size")
                .save(dir.join(format!("{name}.png")))
                .expect("save");
        }
    }
}
