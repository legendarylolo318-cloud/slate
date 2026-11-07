//! Off-thread-safe image helpers. Everything here returns plain pixel buffers
//! (which are `Send`); the `slint::Image` is built on the UI thread.

use slint::{Rgb8Pixel, Rgba8Pixel, SharedPixelBuffer};

/// A decoded cover. Opaque images (every JPEG, PNG without alpha) are kept as RGB, 3 bytes a
/// pixel instead of 4; images with alpha stay RGBA. Cheap to clone and `Send`.
#[derive(Clone)]
pub enum Pixels {
    Rgb(SharedPixelBuffer<Rgb8Pixel>),
    Rgba(SharedPixelBuffer<Rgba8Pixel>),
}

impl Pixels {
    pub fn width(&self) -> u32 {
        match self {
            Self::Rgb(b) => b.width(),
            Self::Rgba(b) => b.width(),
        }
    }

    pub fn height(&self) -> u32 {
        match self {
            Self::Rgb(b) => b.height(),
            Self::Rgba(b) => b.height(),
        }
    }

    /// Bytes the pixels occupy (what the cache's budget counts).
    pub fn byte_size(&self) -> usize {
        let bpp = match self {
            Self::Rgb(_) => 3,
            Self::Rgba(_) => 4,
        };
        self.width() as usize * self.height() as usize * bpp
    }

    /// The `slint::Image` (UI thread). Slint uploads either layout to the same GPU format.
    pub fn into_image(self) -> slint::Image {
        match self {
            Self::Rgb(b) => slint::Image::from_rgb8(b),
            Self::Rgba(b) => slint::Image::from_rgba8(b),
        }
    }

    /// The pixels as RGBA, for code that works on that layout (colour pick, blur, share card).
    /// A copy for RGB; a shared handle for RGBA.
    pub fn to_rgba(&self) -> SharedPixelBuffer<Rgba8Pixel> {
        match self {
            Self::Rgba(b) => b.clone(),
            Self::Rgb(rgb) => {
                let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(rgb.width(), rgb.height());
                for (dst, src) in buf.make_mut_slice().iter_mut().zip(rgb.as_slice()) {
                    *dst = Rgba8Pixel {
                        r: src.r,
                        g: src.g,
                        b: src.b,
                        a: 255,
                    };
                }
                buf
            }
        }
    }

    /// Shrunk to fit `max_edge` pixels on the longer side (the filter and size rule of
    /// [`decode_fit`]); `self` when it already fits.
    pub fn fit(&self, max_edge: u32) -> Self {
        use image::DynamicImage;
        if max_edge == 0 || self.width().max(self.height()) <= max_edge {
            return self.clone();
        }
        let shrunk = match self {
            Self::Rgb(b) => image::RgbImage::from_raw(b.width(), b.height(), b.as_bytes().to_vec())
                .map(|i| {
                    Self::Rgb(rgb_buffer(
                        &DynamicImage::ImageRgb8(i)
                            .thumbnail(max_edge, max_edge)
                            .into_rgb8(),
                    ))
                }),
            Self::Rgba(b) => {
                image::RgbaImage::from_raw(b.width(), b.height(), b.as_bytes().to_vec()).map(|i| {
                    Self::Rgba(rgba_buffer(
                        &DynamicImage::ImageRgba8(i)
                            .thumbnail(max_edge, max_edge)
                            .into_rgba8(),
                    ))
                })
            }
        };
        shrunk.unwrap_or_else(|| self.clone())
    }
}

fn rgb_buffer(img: &image::RgbImage) -> SharedPixelBuffer<Rgb8Pixel> {
    SharedPixelBuffer::clone_from_slice(img.as_raw(), img.width(), img.height())
}

fn rgba_buffer(img: &image::RgbaImage) -> SharedPixelBuffer<Rgba8Pixel> {
    SharedPixelBuffer::clone_from_slice(img.as_raw(), img.width(), img.height())
}

/// Decodes encoded image bytes (JPEG/PNG) into an RGBA buffer at full size.
#[cfg(test)]
fn decode(bytes: &[u8]) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    decode_fit(bytes, 0)
}

/// Decodes encoded image bytes (JPEG/PNG) into an RGBA buffer, first shrinking
/// the image to fit `max_edge` pixels on its longer side, so a small thumbnail
/// does not keep (or upload to the GPU) the full CDN size. `0` keeps the
/// original size.
pub fn decode_fit(bytes: &[u8], max_edge: u32) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let img = image::load_from_memory(bytes).ok()?;
    if max_edge == 0 || img.width().max(img.height()) <= max_edge {
        return Some(to_buffer(img));
    }
    Some(to_buffer(img.thumbnail(max_edge, max_edge)))
}

/// [`decode_fit`] for the image cache: opaque images stay RGB (no alpha byte), images with
/// alpha stay RGBA.
pub fn decode_pixels(bytes: &[u8], max_edge: u32) -> Option<Pixels> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if max_edge == 0 || img.width().max(img.height()) <= max_edge {
        img
    } else {
        img.thumbnail(max_edge, max_edge)
    };
    Some(if img.color().has_alpha() {
        Pixels::Rgba(to_buffer(img))
    } else {
        let rgb = img.into_rgb8();
        Pixels::Rgb(rgb_buffer(&rgb))
    })
}

/// The pixels are written straight into the final buffer, so a JPEG (RGB) is
/// converted and copied once instead of into a temporary and then again.
fn to_buffer(img: image::DynamicImage) -> SharedPixelBuffer<Rgba8Pixel> {
    use image::DynamicImage;

    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(img.width(), img.height());
    let out = buf.make_mut_bytes();
    match img {
        DynamicImage::ImageRgb8(rgb) => {
            let (dst, _) = out.as_chunks_mut::<4>();
            let (src, _) = rgb.as_raw().as_chunks::<3>();
            for (dst, src) in dst.iter_mut().zip(src) {
                *dst = [src[0], src[1], src[2], 255];
            }
        }
        DynamicImage::ImageRgba8(rgba) => out.copy_from_slice(rgba.as_raw()),
        other => out.copy_from_slice(other.into_rgba8().as_raw()),
    }
    buf
}

/// Renders `data` as a QR code with a 4-module quiet zone.
pub fn qr(data: &str) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    use qrcode::{Color, QrCode};
    const SCALE: u32 = 8;
    const QUIET: u32 = 4;
    let code = QrCode::new(data.as_bytes()).ok()?;
    let modules = code.width() as u32;
    let side = (modules + 2 * QUIET) * SCALE;
    let colors = code.to_colors();
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(side, side);
    let stride = side as usize;
    let pixels = buf.make_mut_slice();
    for y in 0..side {
        for x in 0..side {
            let mx = (x / SCALE).checked_sub(QUIET);
            let my = (y / SCALE).checked_sub(QUIET);
            let dark = match (mx, my) {
                (Some(mx), Some(my)) if mx < modules && my < modules => {
                    colors[(my * modules + mx) as usize] == Color::Dark
                }
                _ => false,
            };
            let v = if dark { 0 } else { 255 };
            pixels[y as usize * stride + x as usize] = Rgba8Pixel {
                r: v,
                g: v,
                b: v,
                a: 255,
            };
        }
    }
    Some(buf)
}

/// `m:ss` / `h:mm:ss`.
pub fn format_time(secs: f32) -> String {
    let total = if secs.is_finite() && secs > 0.0 {
        secs as u64
    } else {
        0
    };
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// TIDAL image id → CDN URL. TIDAL only serves 160/320/640/1280 squares, so the
/// requested size snaps up to the next one; full URLs pass through untouched.
pub fn tidal_image_url(id: &str, size: u32) -> String {
    if id.starts_with("http") {
        return id.to_string();
    }
    let size = match size {
        0..=160 => 160,
        161..=320 => 320,
        321..=640 => 640,
        _ => 1280,
    };
    format!(
        "https://resources.tidal.com/images/{}/{size}x{size}.jpg",
        id.replace('-', "/")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_times() {
        assert_eq!(format_time(0.0), "0:00");
        assert_eq!(format_time(65.9), "1:05");
        assert_eq!(format_time(3725.0), "1:02:05");
        assert_eq!(format_time(f32::NAN), "0:00");
        assert_eq!(format_time(-3.0), "0:00");
    }

    #[test]
    fn snaps_sizes_and_passes_urls_through() {
        assert!(tidal_image_url("a-b", 80).ends_with("/160x160.jpg"));
        assert!(tidal_image_url("a-b", 321).ends_with("/640x640.jpg"));
        assert!(tidal_image_url("a-b", 5000).ends_with("/1280x1280.jpg"));
        assert_eq!(tidal_image_url("https://x/y.jpg", 320), "https://x/y.jpg");
    }

    #[test]
    fn builds_cdn_url() {
        assert_eq!(
            tidal_image_url("ab-cd-ef", 320),
            "https://resources.tidal.com/images/ab/cd/ef/320x320.jpg"
        );
    }

    #[test]
    fn qr_is_square_with_quiet_zone() {
        let b = qr("https://link.tidal.com/ABCDE").expect("qr");
        assert_eq!(b.width(), b.height());
        // top-left corner lies in the quiet zone: white
        assert_eq!(b.as_slice()[0].r, 255);
    }

    fn encoded(img: image::DynamicImage, format: image::ImageFormat) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, format).expect("encode");
        out.into_inner()
    }

    /// The two-copy path `decode` replaced; the reference for the tests.
    fn decode_two_copies(bytes: &[u8]) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
        let rgba = image::load_from_memory(bytes).ok()?.to_rgba8();
        let (w, h) = rgba.dimensions();
        Some(SharedPixelBuffer::clone_from_slice(rgba.as_raw(), w, h))
    }

    fn gradient(w: u32, h: u32) -> image::RgbaImage {
        image::RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba([
                (x * 7) as u8,
                (y * 5) as u8,
                (x + y) as u8,
                255 - (x % 50) as u8,
            ])
        })
    }

    #[test]
    fn decode_matches_the_two_copy_path_for_every_pixel_layout() {
        let rgba = gradient(37, 23);
        let cases = [
            encoded(
                image::DynamicImage::ImageRgba8(rgba.clone()),
                image::ImageFormat::Png,
            ),
            encoded(
                image::DynamicImage::ImageRgb8(
                    image::DynamicImage::ImageRgba8(rgba.clone()).to_rgb8(),
                ),
                image::ImageFormat::Png,
            ),
            encoded(
                image::DynamicImage::ImageRgb8(
                    image::DynamicImage::ImageRgba8(rgba.clone()).to_rgb8(),
                ),
                image::ImageFormat::Jpeg,
            ),
            encoded(
                image::DynamicImage::ImageLuma8(image::DynamicImage::ImageRgba8(rgba).to_luma8()),
                image::ImageFormat::Png,
            ),
        ];
        for bytes in &cases {
            let a = decode(bytes).expect("decode");
            let b = decode_two_copies(bytes).expect("reference");
            assert_eq!((a.width(), a.height()), (b.width(), b.height()));
            assert_eq!(a.as_bytes(), b.as_bytes());
        }
        assert!(decode(b"not an image").is_none());
    }

    #[test]
    fn decode_fit_shrinks_to_the_longer_edge_and_never_enlarges() {
        let jpeg = encoded(
            image::DynamicImage::ImageRgba8(gradient(320, 160)),
            image::ImageFormat::Png,
        );
        let small = decode_fit(&jpeg, 80).expect("decode");
        assert_eq!((small.width(), small.height()), (80, 40));
        let full = decode_fit(&jpeg, 0).expect("decode");
        assert_eq!((full.width(), full.height()), (320, 160));
        let same = decode_fit(&jpeg, 1000).expect("decode");
        assert_eq!((same.width(), same.height()), (320, 160));
    }

    #[test]
    fn opaque_covers_stay_rgb_and_alpha_ones_rgba() {
        let rgba = gradient(37, 23);
        let rgb = image::DynamicImage::ImageRgba8(rgba.clone()).to_rgb8();
        let jpeg = encoded(
            image::DynamicImage::ImageRgb8(rgb.clone()),
            image::ImageFormat::Jpeg,
        );
        let png_rgb = encoded(image::DynamicImage::ImageRgb8(rgb), image::ImageFormat::Png);
        let png_rgba = encoded(
            image::DynamicImage::ImageRgba8(rgba),
            image::ImageFormat::Png,
        );
        let grey = encoded(
            image::DynamicImage::ImageLuma8(
                image::DynamicImage::ImageRgba8(gradient(8, 8)).to_luma8(),
            ),
            image::ImageFormat::Png,
        );
        for (bytes, want_rgb) in [
            (&jpeg, true),
            (&png_rgb, true),
            (&png_rgba, false),
            (&grey, true),
        ] {
            let px = decode_pixels(bytes, 0).expect("decode");
            assert_eq!(matches!(px, Pixels::Rgb(_)), want_rgb);
            assert_eq!(
                px.byte_size(),
                (px.width() * px.height()) as usize * if want_rgb { 3 } else { 4 }
            );
            // The same pixels as the RGBA path, whatever the layout.
            let reference = decode(bytes).expect("reference");
            assert_eq!(px.to_rgba().as_bytes(), reference.as_bytes());
        }
        assert!(decode_pixels(b"nope", 0).is_none());
    }

    #[test]
    fn cutting_a_decoded_cover_matches_decoding_it_small() {
        let rgb = image::DynamicImage::ImageRgba8(gradient(320, 160)).to_rgb8();
        let jpeg = encoded(
            image::DynamicImage::ImageRgb8(rgb),
            image::ImageFormat::Jpeg,
        );
        let full = decode_pixels(&jpeg, 0).expect("full");
        for edge in [80, 100, 33] {
            let cut = full.fit(edge);
            let direct = decode_pixels(&jpeg, edge).expect("direct");
            assert_eq!(
                (cut.width(), cut.height()),
                (direct.width(), direct.height())
            );
            assert_eq!(cut.to_rgba().as_bytes(), direct.to_rgba().as_bytes());
        }
        let same = full.fit(1000);
        assert_eq!((same.width(), same.height()), (320, 160));
    }

    #[test]
    #[ignore = "timing; run with --release -- --ignored --nocapture"]
    fn decode_timing() {
        let rgb = image::DynamicImage::ImageRgba8(gradient(640, 640)).to_rgb8();
        let jpeg = encoded(
            image::DynamicImage::ImageRgb8(rgb),
            image::ImageFormat::Jpeg,
        );
        for (name, f) in [
            ("two copies", decode_two_copies as fn(&[u8]) -> _),
            ("one copy", decode as fn(&[u8]) -> _),
        ] {
            let t = std::time::Instant::now();
            for _ in 0..300 {
                std::hint::black_box(f(&jpeg));
            }
            println!("{name}: {:?} per 640x640 jpeg", t.elapsed() / 300);
        }
    }
}
