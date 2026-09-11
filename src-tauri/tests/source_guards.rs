//! Invariants about where certain code may appear. Cheap, and they catch the
//! classes of regression that runtime tests structurally cannot: a test can
//! observe what a proxied client *does*, but not that a second, unproxied one
//! was built somewhere else in the tree.

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

/// A guard whose glob matched nothing is worse than no guard, so prove the
/// walker actually reaches the files every other test in here reasons about.
#[test]
fn the_source_walk_sees_the_files_these_guards_are_about() {
    let sources = rust_sources();
    assert!(
        sources.len() > 20,
        "source walk found only {} files; it is not rooted at src-tauri/src",
        sources.len()
    );
    for expected in ["main.rs", "audio.rs", "proxy.rs", "proxy_http.rs", "lib.rs"] {
        assert!(
            sources.iter().any(|p| file_name(p) == expected),
            "source walk never reached {expected}; these guards are vacuous"
        );
    }
}

#[test]
fn proxy_objects_and_http_clients_are_built_only_in_proxy_http() {
    for f in rust_sources() {
        if file_name(&f) == "proxy_http.rs" {
            continue;
        }
        let body = fs::read_to_string(&f).unwrap();
        assert!(
            !body.contains("reqwest::Proxy::"),
            "{}: construct proxies in proxy_http.rs only",
            f.display()
        );
        for needle in ["reqwest::Client::builder", "reqwest::Client::new"] {
            assert!(
                !body.contains(needle),
                "{}: a client built here bypasses the proxy plan and silently \
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

    assert!(
        offenders.is_empty(),
        "{offenders:?}: Url::set_port drops default ports; build URIs by \
         concatenation in proxy.rs. {EXEMPT} is the only permitted exemption."
    );
    assert!(
        exemption_still_needed,
        "{EXEMPT} no longer calls .set_port(): the exemption in this test is \
         obsolete, so delete the exemption (or this whole test) rather than \
         leaving an allowlist nobody re-reads."
    );
}

/// Mutating the environment is unsound once GTK/glib have threads, and glib
/// reads these through `g_getenv` only after those threads exist. `main.rs`
/// runs single-threaded before `tauri_app_lib::run()`, so it is the only sound
/// place for it.
#[test]
fn environment_is_mutated_only_in_main_before_run() {
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
