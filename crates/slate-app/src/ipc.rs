//! Single-instance enforcement and forwarding of `tidal://` links.
//!
//! The first process listens on a per-user local socket; later launches send it
//! their link (or just a "raise" request) and exit.

use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc;

use interprocess::local_socket::{
    traits::{ListenerExt as _, Stream as _},
    ListenerOptions, Name, Stream,
};

/// Connections read at once; more are closed unread.
const MAX_READERS: usize = 8;

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

/// Linux: a socket file in `$XDG_RUNTIME_DIR`, which only this user can enter.
/// An abstract-namespace socket has no permissions at all: any local account
/// could send it links, or take the name first and keep Slate from starting.
#[cfg(target_os = "linux")]
fn socket_name() -> Option<Name<'static>> {
    use interprocess::local_socket::{GenericFilePath, ToFsName};
    socket_path()?.to_fs_name::<GenericFilePath>().ok()
}

#[cfg(target_os = "linux")]
fn socket_path() -> Option<std::path::PathBuf> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let dir = std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
    let meta = std::fs::metadata(&dir).ok()?;
    let me = std::fs::metadata("/proc/self").ok()?.uid();
    if !meta.is_dir() || meta.uid() != me || meta.permissions().mode() & 0o077 != 0 {
        tracing::warn!("XDG_RUNTIME_DIR is not private; single-instance disabled");
        return None;
    }
    Some(dir.join("slate.sock"))
}

/// Windows: a named pipe in this user's name. (Pipe ACLs are tracked in REVIEW.md.)
#[cfg(not(target_os = "linux"))]
fn socket_name() -> Option<Name<'static>> {
    use interprocess::local_socket::{GenericNamespaced, ToNsName};
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default();
    format!("slate-{user}.sock")
        .to_ns_name::<GenericNamespaced>()
        .ok()
}

/// A socket file left by a crashed instance refuses connections; remove it so
/// the listener can bind. Only called after connecting to it failed.
/// (Windows pipes vanish with their last handle.)
fn clear_stale() {
    #[cfg(target_os = "linux")]
    if let Some(path) = socket_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// Tries to hand `message` to a running instance.
fn forward(name: &Name<'_>, message: &str) -> bool {
    match Stream::connect(name.borrow()) {
        Ok(mut s) => {
            // Windows pipe names are global: another account could have
            // created ours first. Only hand links to a server we own.
            #[cfg(windows)]
            if !winsec::server_is_same_user(&s) {
                tracing::warn!("single-instance pipe is held by another user; not forwarding");
                return false;
            }
            s.write_all(format!("{message}\n").as_bytes()).is_ok()
        }
        Err(_) => false,
    }
}

pub fn claim(link_arg: Option<&str>) -> Claim {
    let Some(name) = socket_name() else {
        return Claim::Unavailable;
    };
    if forward(&name, link_arg.unwrap_or(RAISE)) {
        return Claim::Forwarded;
    }
    clear_stale();
    let options = ListenerOptions::new().name(name);
    #[cfg(windows)]
    let options = winsec::owner_only(options);
    let listener = match options.create_sync() {
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
            // Each connection is read on its own thread so a client that
            // connects and never writes cannot hold up later links; the cap
            // keeps a flood of idle connections from costing more than that.
            let busy = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            for conn in listener.incoming().flatten() {
                use std::sync::atomic::Ordering;
                if busy.fetch_add(1, Ordering::SeqCst) >= MAX_READERS {
                    busy.fetch_sub(1, Ordering::SeqCst);
                    continue; // dropped: the sender sees the connection close
                }
                let (tx, held) = (tx.clone(), std::sync::Arc::clone(&busy));
                let reader = std::thread::Builder::new()
                    .name("slate-ipc-read".into())
                    .spawn(move || {
                        let mut line = String::new();
                        // One short line per connection; capped so a rogue client cannot balloon memory.
                        let mut reader = BufReader::new(conn).take_line_limited();
                        if reader.read_line(&mut line).is_ok() {
                            let msg = line.trim().to_string();
                            if !msg.is_empty() {
                                let _ = tx.send(msg);
                            }
                        }
                        held.fetch_sub(1, Ordering::SeqCst);
                    });
                if reader.is_err() {
                    busy.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
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

/// Windows: the pipe's access list, and checking who serves it.
#[cfg(windows)]
mod winsec {
    use interprocess::local_socket::{traits::StreamCommon as _, ListenerOptions, Stream};
    use interprocess::os::windows::{
        local_socket::ListenerOptionsExt, security_descriptor::SecurityDescriptor,
    };
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        EqualSid, GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// Protected DACL with one entry: full access for the pipe's owner (the
    /// user who started Slate). The default would let other accounts connect.
    const OWNER_ONLY: &str = "D:P(A;;GA;;;OW)";

    pub fn owner_only(options: ListenerOptions<'_>) -> ListenerOptions<'_> {
        match widestring::U16CString::from_str(OWNER_ONLY)
            .ok()
            .and_then(|s| SecurityDescriptor::deserialize(&s).ok())
        {
            Some(sd) => options.security_descriptor(sd),
            None => {
                tracing::warn!("could not build the pipe's access list; using the default");
                options
            }
        }
    }

    pub fn server_is_same_user(stream: &Stream) -> bool {
        stream
            .peer_creds()
            .ok()
            .and_then(|c| c.pid())
            .is_some_and(is_same_user)
    }

    /// The process's user SID, in a buffer aligned for `TOKEN_USER`.
    fn token_user(process: HANDLE) -> Option<Vec<u64>> {
        // SAFETY: the token handle is opened and closed here; the buffer is
        // sized from the first call and 8-byte aligned for TOKEN_USER.
        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(process, TOKEN_QUERY, &mut token).ok()?;
            let mut len = 0u32;
            let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
            let mut buf = vec![0u64; (len as usize).div_ceil(8)];
            let got = GetTokenInformation(
                token,
                TokenUser,
                Some(buf.as_mut_ptr().cast()),
                len,
                &mut len,
            );
            let _ = CloseHandle(token);
            got.ok()?;
            Some(buf)
        }
    }

    pub fn is_same_user(pid: u32) -> bool {
        // SAFETY: handles are closed before return; both buffers hold a
        // TOKEN_USER written by GetTokenInformation and outlive the SID reads.
        unsafe {
            let Some(mine) = token_user(GetCurrentProcess()) else {
                return false;
            };
            let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return false;
            };
            let theirs = token_user(process);
            let _ = CloseHandle(process);
            let Some(theirs) = theirs else {
                return false;
            };
            let a = &*mine.as_ptr().cast::<TOKEN_USER>();
            let b = &*theirs.as_ptr().cast::<TOKEN_USER>();
            EqualSid(a.User.Sid, b.User.Sid).is_ok()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use interprocess::local_socket::{
            traits::{ListenerExt as _, Stream as _},
            GenericNamespaced, ToNsName,
        };

        #[test]
        fn this_process_is_its_own_user() {
            assert!(is_same_user(std::process::id()));
        }

        #[test]
        fn owner_only_pipe_accepts_its_owner_and_reports_the_server() {
            let name = format!("slate-test-{}.sock", std::process::id())
                .to_ns_name::<GenericNamespaced>()
                .unwrap();
            let listener = owner_only(ListenerOptions::new().name(name.borrow()))
                .create_sync()
                .unwrap();
            let accept = std::thread::spawn(move || listener.incoming().next().map(|c| c.is_ok()));
            let client = Stream::connect(name).unwrap();
            assert!(server_is_same_user(&client));
            assert_eq!(accept.join().unwrap(), Some(true));
        }
    }
}
