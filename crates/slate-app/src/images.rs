//! Off-thread-safe image helpers. Everything here returns plain pixel buffers
//! (which are `Send`); the `slint::Image` is built on the UI thread.

use slint::{Rgba8Pixel, SharedPixelBuffer};

/// Decodes encoded image bytes (JPEG/PNG) into an RGBA buffer.
pub fn decode(bytes: &[u8]) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let rgba = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some(SharedPixelBuffer::clone_from_slice(rgba.as_raw(), w, h))
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

/// TIDAL image UUID → CDN URL.
pub fn tidal_image_url(uuid: &str, size: u32) -> String {
    format!(
        "https://resources.tidal.com/images/{}/{size}x{size}.jpg",
        uuid.replace('-', "/")
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
}
