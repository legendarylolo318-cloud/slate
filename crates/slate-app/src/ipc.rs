//! Single-instance enforcement and forwarding of `tidal://` links.
//!
//! The first process listens on a per-user local socket; later launches send it
//! their link (or just a "raise" request) and exit.

use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc;

use interprocess::local_socket::{
    traits::{ListenerExt as _, Stream as _},
    GenericNamespaced, ListenerOptions, Stream, ToNsName,
};

/// What the first process receives from later launches.
pub const RAISE: &str = "RAISE";

pub enum Claim {
    /// We are the only instance; messages from later launches arrive on the receiver.
    Primary(mpsc::Receiver<String>),
    /// An instance was already running and got our message.
    Forwarded,
    /// IPC is unavailable; run without single-instance behaviour.
    Unavailable,
}

fn socket_name() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    format!("slate-{user}.sock")
}

/// Tries to hand `message` to a running instance.
fn forward(name: &interprocess::local_socket::Name<'_>, message: &str) -> bool {
    match Stream::connect(name.borrow()) {
        Ok(mut s) => s.write_all(format!("{message}\n").as_bytes()).is_ok(),
        Err(_) => false,
    }
}

pub fn claim(link_arg: Option<&str>) -> Claim {
    let Ok(name) = socket_name().to_ns_name::<GenericNamespaced>() else {
        return Claim::Unavailable;
    };
    if forward(&name, link_arg.unwrap_or(RAISE)) {
        return Claim::Forwarded;
    }
    let listener = match ListenerOptions::new().name(name).create_sync() {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("single-instance socket unavailable: {e}");
            return Claim::Unavailable;
        }
    };
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("slate-ipc".into())
        .spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut line = String::new();
                // One short line per connection; cap it so a rogue client cannot balloon memory.
                let mut reader = BufReader::new(conn).take_line_limited();
                if reader.read_line(&mut line).is_ok() {
                    let msg = line.trim().to_string();
                    if !msg.is_empty() && tx.send(msg).is_err() {
                        return;
                    }
                }
            }
        });
    if spawned.is_err() {
        return Claim::Unavailable;
    }
    Claim::Primary(rx)
}

/// `BufReader` that refuses lines longer than a link could plausibly be.
trait LimitedLine<R: std::io::Read> {
    fn take_line_limited(self) -> BufReader<std::io::Take<R>>;
}

impl<R: std::io::Read> LimitedLine<R> for BufReader<R> {
    fn take_line_limited(self) -> BufReader<std::io::Take<R>> {
        BufReader::new(self.into_inner().take(2048))
    }
}
