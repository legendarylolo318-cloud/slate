//! Settings → General "Notify on track change": a desktop notification with the title,
//! artist and cover when a new track starts and Slate's window is not in front. Linux only
//! (`org.freedesktop.Notifications`); off by default, and then nothing connects to D-Bus.
//!
//! Each notification replaces the previous one, so skipping through ten tracks leaves one
//! notification, not ten.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use zbus::zvariant::{StructureBuilder, Value};

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;
}

static ENABLED: AtomicBool = AtomicBool::new(false);

/// The session connection (made on first use) and the id of the last notification shown.
static LAST: tokio::sync::Mutex<Option<(zbus::Connection, u32)>> =
    tokio::sync::Mutex::const_new(None);

/// How long the notification stays up, in ms (the server may ignore it).
const EXPIRE_MS: i32 = 5000;

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// A cover for the notification: RGBA pixels, `width * height * 4` bytes.
pub struct Cover {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The body is markup on most servers: a title with `&` or `<` must not break it.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// The `image-data` hint, `(iiibiiay)`: width, height, row stride, alpha, bits per sample,
/// channels, pixels. `None` for a cover whose sizes do not add up.
fn image_hint(c: Cover) -> Option<Value<'static>> {
    let w = i32::try_from(c.width).ok()?;
    let h = i32::try_from(c.height).ok()?;
    let stride = w.checked_mul(4)?;
    if c.rgba.len() != usize::try_from(stride).ok()? * usize::try_from(h).ok()? {
        return None;
    }
    StructureBuilder::new()
        .add_field(w)
        .add_field(h)
        .add_field(stride)
        .add_field(true)
        .add_field(8i32)
        .add_field(4i32)
        .add_field(c.rgba)
        .build()
        .ok()
        .map(Value::from)
}

/// Shows (or replaces) the track notification. Failures are logged and forgotten: a desktop
/// without a notification server just gets none.
pub async fn show(title: &str, artist: &str, album: &str, cover: Option<Cover>) {
    if !enabled() {
        return;
    }
    let mut last = LAST.lock().await;
    if last.is_none() {
        match zbus::Connection::session().await {
            Ok(conn) => *last = Some((conn, 0)),
            Err(e) => {
                tracing::debug!("no session bus for notifications: {e}");
                return;
            }
        }
    }
    let Some((conn, prev)) = last.as_mut() else {
        return;
    };
    let proxy = match NotificationsProxy::new(conn).await {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!("no notification server: {e}");
            return;
        }
    };
    let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
    hints.insert("desktop-entry", Value::from(super::APP_ID));
    // A media notification, not something that needs attention: no sound, not kept.
    hints.insert("category", Value::from("x-gnome.music"));
    hints.insert("suppress-sound", Value::from(true));
    hints.insert("transient", Value::from(true));
    if let Some(img) = cover.and_then(image_hint) {
        hints.insert("image-data", img);
    }
    let body = if album.is_empty() {
        escape(artist)
    } else {
        format!("{}\n{}", escape(artist), escape(album))
    };
    match proxy
        .notify(
            "Slate",
            *prev,
            super::APP_ID,
            title,
            &body,
            &[],
            hints,
            EXPIRE_MS,
        )
        .await
    {
        Ok(id) => *prev = id,
        Err(e) => {
            tracing::debug!("track notification not shown: {e}");
            // The bus may have gone (session restart): connect again next time.
            *last = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_in_names_is_escaped() {
        assert_eq!(escape("Tom & Jerry <live>"), "Tom &amp; Jerry &lt;live&gt;");
    }

    #[test]
    fn a_cover_with_the_wrong_size_is_left_out() {
        let bad = Cover {
            width: 2,
            height: 2,
            rgba: vec![0; 15],
        };
        assert!(image_hint(bad).is_none());
        let good = Cover {
            width: 2,
            height: 2,
            rgba: vec![0; 16],
        };
        assert!(image_hint(good).is_some());
    }
}
