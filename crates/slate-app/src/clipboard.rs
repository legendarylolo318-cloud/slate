//! One clipboard handle for the whole process, made on the first copy and kept.
//!
//! On X11 the copied data lives in the process that owns the selection: dropping the
//! `arboard::Clipboard` right after `set_text` (as every copy did) lost it at once unless a
//! clipboard manager took it over, so pasting gave nothing. Keeping the handle keeps the
//! selection served until another application copies something. On Wayland (data-control)
//! and Windows the copy outlived the handle already; keeping it there costs nothing.

use std::sync::Mutex;

static CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);

fn with<R>(
    f: impl FnOnce(&mut arboard::Clipboard) -> Result<R, arboard::Error>,
) -> Result<R, arboard::Error> {
    let mut g = CLIPBOARD.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(arboard::Clipboard::new()?);
    }
    let r = match g.as_mut() {
        Some(c) => f(c),
        None => return Err(arboard::Error::ClipboardNotSupported),
    };
    if r.is_err() {
        // A broken connection (the X server restarted…) is made again on the next copy.
        *g = None;
    }
    r
}

/// Copies `text`. Call from a blocking thread: the first call connects to the display.
pub fn set_text(text: String) -> Result<(), arboard::Error> {
    with(|c| c.set_text(text))
}

/// Copies an RGBA image.
pub fn set_image(width: usize, height: usize, rgba: &[u8]) -> Result<(), arboard::Error> {
    with(|c| {
        c.set_image(arboard::ImageData {
            width,
            height,
            bytes: std::borrow::Cow::Borrowed(rgba),
        })
    })
}
