//! Full-screen lyrics: the blurred backdrop. It is made once per track from the cover that is
//! already decoded for the player bar, shrunk to at most 128 px first so the blur costs
//! microseconds, and scaled back up by the renderer. Nothing is computed unless the view is
//! open, and the image is dropped when it closes.

use std::sync::atomic::{AtomicBool, Ordering};

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use crate::backend::Backend;
use crate::bootstrap::AppWindow;
use crate::imgcache::Pixels;

/// Longest edge of the image that gets blurred.
pub const THUMB: u32 = 128;
const SIGMA: f32 = 6.0;
/// How much of the cover's brightness is kept (the lines need to stay readable).
const KEEP: f32 = 0.55;

static OPEN: AtomicBool = AtomicBool::new(false);

/// A small, blurred, darkened copy of `cover`; `None` for an empty image.
pub fn backdrop(cover: &Pixels) -> Option<Pixels> {
    let (w, h) = (cover.width(), cover.height());
    if w == 0 || h == 0 {
        return None;
    }
    let src = image::RgbaImage::from_raw(w, h, cover.as_bytes().to_vec())?;
    let small = if w.max(h) > THUMB {
        image::imageops::thumbnail(
            &src,
            (w * THUMB / w.max(h)).max(1),
            (h * THUMB / w.max(h)).max(1),
        )
    } else {
        src
    };
    let mut blurred = image::imageops::blur(&small, SIGMA);
    for p in blurred.pixels_mut() {
        for c in &mut p.0[..3] {
            *c = (f32::from(*c) * KEEP) as u8;
        }
        p.0[3] = 255;
    }
    let mut out = SharedPixelBuffer::<Rgba8Pixel>::new(blurred.width(), blurred.height());
    out.make_mut_bytes().copy_from_slice(blurred.as_raw());
    Some(out)
}

impl Backend {
    /// The view opened or closed (UI thread callback).
    pub fn lyrics_full_changed(&self, open: bool) {
        OPEN.store(open, Ordering::Relaxed);
        if open {
            let px = self.shared.cover_px.lock().ok().and_then(|p| p.clone());
            self.lyrics_full_backdrop(px);
        } else {
            self.ui(|w: AppWindow| w.set_lyrics_backdrop(Image::default()));
        }
    }

    /// The playing track has no cover: the previous track's backdrop must not stay.
    pub(crate) fn lyrics_full_no_cover(&self) {
        if OPEN.load(Ordering::Relaxed) {
            self.ui(|w: AppWindow| w.set_lyrics_backdrop(Image::default()));
        }
    }

    /// A new cover arrived while the view is open.
    pub(crate) fn lyrics_full_cover(&self, px: &Pixels) {
        if OPEN.load(Ordering::Relaxed) {
            self.lyrics_full_backdrop(Some(px.clone()));
        }
    }

    fn lyrics_full_backdrop(&self, px: Option<Pixels>) {
        let this = self.clone();
        let id = self.current_track_id();
        self.rt.spawn(async move {
            let img = match px {
                Some(px) => tokio::task::spawn_blocking(move || backdrop(&px))
                    .await
                    .ok()
                    .flatten(),
                None => None,
            };
            if !OPEN.load(Ordering::Relaxed) || this.current_track_id() != id {
                return;
            }
            this.ui(move |w| {
                w.set_lyrics_backdrop(img.map(Image::from_rgba8).unwrap_or_default());
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, c: [u8; 4]) -> Pixels {
        let mut b = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
        for p in b.make_mut_slice() {
            *p = Rgba8Pixel {
                r: c[0],
                g: c[1],
                b: c[2],
                a: c[3],
            };
        }
        b
    }

    #[test]
    fn big_covers_are_shrunk_to_the_thumbnail_size() {
        let out = backdrop(&solid(640, 640, [200, 100, 50, 255])).expect("backdrop");
        assert_eq!((out.width(), out.height()), (THUMB, THUMB));
        let wide = backdrop(&solid(640, 320, [0, 0, 0, 255])).expect("backdrop");
        assert_eq!((wide.width(), wide.height()), (THUMB, THUMB / 2));
    }

    #[test]
    fn small_covers_keep_their_size_and_are_darkened() {
        let out = backdrop(&solid(64, 64, [200, 200, 200, 255])).expect("backdrop");
        assert_eq!(out.width(), 64);
        let p = out.as_slice()[64 * 32 + 32];
        assert!(p.r < 200 && p.r > 80, "{}", p.r);
        assert_eq!(p.a, 255);
    }

    #[test]
    fn an_empty_cover_gives_nothing() {
        assert!(backdrop(&SharedPixelBuffer::<Rgba8Pixel>::new(0, 0)).is_none());
    }
}
