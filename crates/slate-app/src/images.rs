//! Off-thread-safe image helpers. Everything here returns plain pixel buffers
//! (which are `Send`); the `slint::Image` is built on the UI thread.

use slint::{Rgba8Pixel, SharedPixelBuffer};

/// Decodes encoded image bytes (JPEG/PNG) into an RGBA buffer. The pixels are
/// written straight into the final buffer, so a JPEG (RGB) is converted and
/// copied once instead of converted into a temporary and then copied again.
pub fn decode(bytes: &[u8]) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    use image::DynamicImage;

    let img = image::load_from_memory(bytes).ok()?;
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
    Some(buf)
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
