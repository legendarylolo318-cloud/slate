//! Invariants about where certain code may appear. Cheap, and they catch the
//! classes of regression that runtime tests structurally cannot: a test can
//! observe what a proxied client *does*, but not that a second, unproxied one
//! was built somewhere else in the tree.
//!
//! Known limits of these guards, so nobody mistakes them for more than they are:
//!
//! - The exemptions are keyed on **basename**, not path. A future
//!   `src/mcp/proxy_http.rs` or `src/commands/audio.rs` would inherit the
//!   exemption from anywhere in the tree, and this crate already has duplicate
//!   basenames (`mod.rs` several times over). Tighten to a full relative path
//!   the day a second file wants one of these names.
//! - Only `src/` is scanned. `src-tauri/tests/` and any build script are
//!   unguarded; a stray client built in a test helper would not be caught.
//! - This is substring matching over source text, not parsing. It raises the
//!   cost of a bypass; it does not make one impossible.
//!
//! Still owed, and deliberately not guarded here: no `window.open` fallbacks in
//! the frontend. `src/components/Login.tsx` has five live ones, each a `catch`
//! after `openUrl` from `@tauri-apps/plugin-opener` fails, and a `window.open`
//! escapes the proxied transport entirely. Removing them is a frontend task;
//! adding the guard before that lands would only break a green suite. When they
//! are gone, guard it — the natural home is a frontend lint, since these tests
//! scan Rust only.

use std::fs;
use std::path::{Path, PathBuf};

fn rust_sources() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in fs::read_dir(dir).expect("read_dir").flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(Path::new("src"), &mut out);
    out
}

fn file_name(p: &Path) -> String {
    p.file_name().unwrap().to_string_lossy().to_string()
}

/// Does `body` mention `ident` as its own path segment?
///
/// Plain `body.contains("Client::new")` is useless here: `TidalClient::new` and
/// `DiscordIpcClient::new` both contain it, and `ScreenSaverProxy::new` (six
/// zbus proxies in `idle_inhibit/dbus.rs`) contains `Proxy::new`. Requiring the
/// preceding byte not to be an identifier character keeps `reqwest::Client::new`
/// and a bare `Client::new` while dropping `SomethingClient::new` — which is
/// the distinction the guards actually care about.
fn mentions_path_segment(body: &str, ident: &str) -> bool {
    body.match_indices(ident).any(|(at, _)| {
        at == 0
            || !body.as_bytes()[..at]
                .last()
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
    })
}

/// A guard whose glob matched nothing is worse than no guard, so prove the walk
/// reaches the files every other test in here reasons about — including nested
/// ones, since losing the `is_dir()` recursion would still leave the 21
/// top-level files and a green suite while `commands/`, `mcp/`, `scrobble/` and
/// `tidal_report/` went unscanned. That is exactly where a stray client lives.
#[test]
fn the_source_walk_recurses_and_sees_the_files_these_guards_are_about() {
    let sources = rust_sources();
    assert!(
        sources.len() > 40,
        "source walk found only {} files; src/ has 21 at the top level and 42 \
         below it, so this many means the recursion is gone or the walk is not \
         rooted at src-tauri/src",
        sources.len()
    );
    for expected in ["main.rs", "audio.rs", "proxy.rs", "proxy_http.rs", "lib.rs"] {
        assert!(
            sources.iter().any(|p| file_name(p) == expected),
            "source walk never reached {expected}; these guards are vacuous"
        );
    }
    // Nested, and one of them three levels deep, so a single-level walk fails.
    for expected in [
        "commands/overlay.rs",
        "scrobble/musicbrainz.rs",
        "tidal_report/event.rs",
        "mcp/tools/catalog.rs",
    ] {
        assert!(
            sources
                .iter()
                .any(|p| p.to_string_lossy().replace('\\', "/").ends_with(expected)),
            "source walk never reached {expected}; it is not recursing, so every \
             other guard here silently skips the subdirectories"
        );
    }
}

/// Every `reqwest::Client` in this process must come from `proxy_http.rs`,
/// because a client built anywhere else has no proxy attached and egresses
/// direct — the silent degradation this whole design exists to prevent.
///
/// The import is guarded as well as the call. `use reqwest::Client;` is what
/// makes a bare `Client::new()` compile, so refusing the import is the cheapest
/// place to stop it; the alternative is chasing every spelling
/// (`reqwest::blocking::Client::new`, `ClientBuilder::new`, an aliased import)
/// through substring matching forever.
#[test]
fn proxy_objects_and_http_clients_are_built_only_in_proxy_http() {
    for f in rust_sources() {
        if file_name(&f) == "proxy_http.rs" {
            continue;
        }
        let body = fs::read_to_string(&f).unwrap();

        for line in body.lines() {
            let t = line.trim_start();
            if t.starts_with("use reqwest::") && (t.contains("Client") || t.contains("Proxy")) {
                panic!(
                    "{}: `{}` — importing reqwest's Client or Proxy here is what \
                     makes an unproxied `Client::new()` possible. Spell the type \
                     `reqwest::Client` inline if a signature needs it; build it \
                     in proxy_http.rs.",
                    f.display(),
                    t.trim_end()
                );
            }
        }

        for ident in ["Proxy::", "Client::builder", "Client::new", "ClientBuilder"] {
            assert!(
                !mentions_path_segment(&body, ident),
                "{}: `{ident}` here bypasses the proxy plan and silently \
                 egresses direct; build it in proxy_http.rs",
                f.display()
            );
        }
    }
}

/// `Url::set_port` is a trap for proxy URIs: it drops the port from the
/// serialized string whenever it equals the scheme default, so an HTTP proxy on
/// port 80 turns into `http://host/` and the port is lost. URIs are built by
/// concatenation in `proxy.rs` instead.
///
/// `audio.rs` is the sole exemption, and only because its `gstreamer_proxy_uri`
/// helper *is* that defect, awaiting deletion with the rest of the GStreamer
/// proxy helpers. The exemption asserts its own reason for existing: when
/// `audio.rs` stops matching, this test fails and tells you to delete it.
///
/// Both conditions are collected before anything is asserted. Failing on the
/// first would hide the obsolescence signal in the case where `audio.rs` is
/// cleaned up and a new offender appears in the same change.
#[test]
fn set_port_is_confined_to_the_one_helper_that_is_slated_for_deletion() {
    const EXEMPT: &str = "audio.rs";

    let mut offenders = Vec::new();
    let mut exemption_still_needed = false;

    for f in rust_sources() {
        if !fs::read_to_string(&f).unwrap().contains(".set_port(") {
            continue;
        }
        if file_name(&f) == EXEMPT {
            exemption_still_needed = true;
        } else {
            offenders.push(f.display().to_string());
        }
    }

    let mut problems = Vec::new();
    if !offenders.is_empty() {
        problems.push(format!(
            "{offenders:?}: Url::set_port drops default ports; build URIs by \
             concatenation in proxy.rs. {EXEMPT} is the only permitted exemption."
        ));
    }
    if !exemption_still_needed {
        problems.push(format!(
            "{EXEMPT} no longer calls .set_port(): the exemption in this test is \
             obsolete, so delete the exemption (or this whole test) rather than \
             leaving an allowlist nobody re-reads."
        ));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Mutating the environment is unsound once GTK/glib have threads, and glib
/// reads these back through `g_getenv` only after those threads exist. `main()`
/// runs single-threaded before `tauri_app_lib::run()`, so it is the only sound
/// place for it.
///
/// This checks location, not ordering: it cannot tell whether the call in
/// `main.rs` actually precedes `run()`. Reviewing that is still on you.
#[test]
fn environment_is_mutated_only_in_main() {
    for f in rust_sources() {
        if file_name(&f) == "main.rs" {
            continue;
        }
        let body = fs::read_to_string(&f).unwrap();
        for needle in ["env::set_var", "env::remove_var"] {
            assert!(
                !body.contains(needle),
                "{}: {needle} is unsound once GTK/glib threads exist; move it to main.rs",
                f.display()
            );
        }
    }
}
