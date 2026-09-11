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
//! - Specific to `the_startup_proxy_scrub_stays_gated_on_the_launch_sidecar`,
//!   and accepted rather than chased: the guard matches the literal prefix
//!   `if should_scrub_proxy_env(` without inspecting the argument, so a
//!   hand-written `if should_scrub_proxy_env(Some((true, "http".into())))`
//!   passes while scrubbing unconditionally;
//!   `SCRUBBED_PROXY_ENV_VARS.iter().take(1)`
//!   passes; and renaming the loop's binding breaks the guard, though it fails
//!   red rather than green. Every one of those takes deliberate effort, and a
//!   substring guard is the wrong tool for stopping an author who is trying.
//!   These guards exist to catch the accidental deletion and the innocent
//!   refactor.
//! - Same test, same acceptance: the capture assertions count no occurrences,
//!   so a commented-out decoy `remember_scrubbed_env(` inside the gate would
//!   satisfy both of them while the live call sat below the removal loop.
//! - `the_mirrored_reqwest_major_version_is_still_what_we_pin` matches the
//!   literal `0.11`, so pinning the dependency exactly (`version = "0.11.27"`)
//!   fails it spuriously. Red rather than green, so it is safe — just noisy,
//!   and the fix is to widen the match when someone actually pins that way.
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

        // `Client::default` is not padding: reqwest's Default impl is literally
        // `Self::new()` for both the async and blocking clients, so it builds a
        // fully functional unproxied client.
        for ident in [
            "Proxy::",
            "Client::builder",
            "Client::new",
            "Client::default",
            "ClientBuilder",
        ] {
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
/// `Url::parse` is the same trap one step earlier and is banned with it, which
/// is what the spec asked for: `Url::parse("http://h:80/")` normalises the port
/// away on its own, so reaching for it to "validate" a proxy URI reintroduces
/// the defect without ever calling the setter. The damage is invisible to a
/// string comparison — libcurl was measured dialling 1080 — so the lint is the
/// guard, not a test of the output.
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
fn the_url_port_manglers_are_confined_to_the_one_helper_slated_for_deletion() {
    const EXEMPT: &str = "audio.rs";

    let mut offenders = Vec::new();
    let mut exemption_still_needed = false;

    for f in rust_sources() {
        let body = fs::read_to_string(&f).unwrap();
        // `Url::parse` as a path segment, so `TidalUrl::parse` and the like do
        // not trip it; `.set_port(` needs no such care.
        if !body.contains(".set_port(") && !mentions_path_segment(&body, "Url::parse") {
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
            "{offenders:?}: Url::set_port and Url::parse both drop a port equal \
             to the scheme default; build URIs by concatenation in proxy.rs. \
             {EXEMPT} is the only permitted exemption."
        ));
    }
    if !exemption_still_needed {
        problems.push(format!(
            "{EXEMPT} no longer calls .set_port() or Url::parse: the exemption in this test is \
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

/// The scrub must stay *gated*. `environment_is_mutated_only_in_main` above
/// asserts where the mutation may live, never that it is conditional — so
/// inverting the `if`, deleting it, or pointing the loop at a different array
/// leaves that guard, and every runtime test, green. `should_scrub_proxy_env`
/// is pure and well covered, but nothing else ties it to the call site.
///
/// The failure this catches is the one the whole design exists to prevent:
/// scrubbing while the user's proxy toggle is off deletes the system proxy
/// configuration of someone behind a corporate proxy, and their traffic
/// silently goes direct.
///
/// Lexical containment is checked by counting braces from the gate's own
/// block, not by proximity, so a `remove_var` moved out of the `if` and left
/// sitting next to it still fails.
#[test]
fn the_startup_proxy_scrub_stays_gated_on_the_launch_sidecar() {
    let body = fs::read_to_string("src/main.rs").expect("src/main.rs");

    let removals = body.match_indices("env::remove_var").count();
    assert_eq!(
        removals, 1,
        "src/main.rs has {removals} `env::remove_var` calls; this guard reasons \
         about exactly one, so a second would be unchecked"
    );
    let removal = body.find("env::remove_var").unwrap();

    // A negated or renamed condition does not match, which is the point: the
    // inverted gate fails here rather than silently passing containment.
    let gate = body.find("if should_scrub_proxy_env(").unwrap_or_else(|| {
        panic!(
            "no `if should_scrub_proxy_env(` in src/main.rs: the startup scrub \
             is ungated, negated, or renamed. It must run only when the launch \
             sidecar says SONE is proxying — `Direct` means the system's own \
             configuration applies and must be left alone."
        )
    });

    let loop_at = body
        .find("for v in tauri_app_lib::proxy::SCRUBBED_PROXY_ENV_VARS")
        .unwrap_or_else(|| {
            panic!(
                "the scrub loop in src/main.rs does not iterate \
                 `tauri_app_lib::proxy::SCRUBBED_PROXY_ENV_VARS`: that array \
                 is the audited removal list, and a different one scrubs the \
                 wrong variables — `PROXY_ENV_VARS` in particular is the wider \
                 capture list, and removing all of it downgrades the surfaces \
                 no stage has taken over yet"
            )
        });

    assert!(
        block_of(&body, gate).contains(&removal),
        "src/main.rs: `env::remove_var` is not inside the \
         `if should_scrub_proxy_env(...)` block. Sitting beside the gate is not \
         being gated — it scrubs on every launch."
    );
    assert!(
        block_of(&body, loop_at).contains(&removal),
        "src/main.rs: `env::remove_var` is not inside the `for v in \
         SCRUBBED_PROXY_ENV_VARS` loop, so it is removing something other \
         than the audited list"
    );

    // The capture is the same shape of hole one level down: deleting it, or
    // moving it after the loop, leaves every runtime test green because the
    // `proxy_http` tests supply the captured values explicitly. Losing it means
    // a user who turns SONE's proxy off mid-session has their system proxy
    // configuration simply gone for the rest of the session.
    let capture = body
        .find("proxy::remember_scrubbed_env(")
        .unwrap_or_else(|| {
            panic!(
                "src/main.rs never calls `remember_scrubbed_env`: the values \
                 about to be removed are the user's own proxy configuration, \
                 and the `Direct` route hands them back. Without the capture \
                 turning SONE's proxy off mid-session sends their traffic \
                 direct instead of through their system's proxy."
            )
        });
    assert!(
        block_of(&body, gate).contains(&capture),
        "src/main.rs: `remember_scrubbed_env` is outside the \
         `if should_scrub_proxy_env(...)` block, so it records an environment \
         nothing is about to remove"
    );
    assert!(
        capture < removal,
        "src/main.rs: `remember_scrubbed_env` runs at byte {capture}, after the \
         `env::remove_var` at {removal}. Capturing after removal captures \
         nothing — it must read the variables while they are still set."
    );
}

/// The byte range of the `{ … }` block that opens after `from`, brace-counted.
///
/// Good enough for this file and no more: it does not know about braces inside
/// strings, comments or char literals. `main.rs` has none between these gates
/// and their bodies, and the guard is about raising the cost of an ungated
/// scrub, not about parsing Rust.
fn block_of(body: &str, from: usize) -> std::ops::Range<usize> {
    let bytes = body.as_bytes();
    let open = from
        + body[from..]
            .find('{')
            .expect("a gate with no block in src/main.rs");
    let mut depth = 0usize;
    for (i, b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return open..i;
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced braces from byte {from} in src/main.rs");
}

/// The proxy save must claim its position in the client cell's write order
/// beside the write to disk, not inside the closure that builds the client.
///
/// Both orderings type-check and both pass every runtime test, because the
/// difference only shows under two overlapping saves. Claiming inside
/// `spawn_blocking` makes the cell's order the order in which the blocking
/// pool happened to run the closures, so a save that persisted an enabled
/// proxy can be overwritten in the cell by an earlier save's `Direct` client —
/// the user reads "proxy on" and egresses from their real address.
///
/// Lexical containment again: a `claim()` moved back inside the closure is
/// what this catches, and proximity would not.
#[test]
fn the_proxy_save_claims_its_generation_beside_the_write_not_inside_the_build() {
    let body = fs::read_to_string("src/commands/utility.rs").expect("src/commands/utility.rs");

    let claims = body.match_indices(".claim()").count();
    assert_eq!(
        claims, 1,
        "src/commands/utility.rs has {claims} `.claim()` calls; this guard          reasons about exactly one, so a second would be unchecked"
    );
    let claim = body.find(".claim()").unwrap_or_else(|| {
        panic!(
            "no `.claim()` in src/commands/utility.rs: the proxy save is back              to letting `apply` claim for itself, which puts the cell's write              order back in the hands of the blocking pool"
        )
    });

    // The call, not the word: the comment above the claim explains the hazard
    // in terms of `spawn_blocking`, and matching that would find the prose.
    let spawn = body.find("tokio::task::spawn_blocking(").unwrap_or_else(|| {
        panic!(
            "no `tokio::task::spawn_blocking(` in src/commands/utility.rs: the              client build has moved, and this guard no longer knows where the              claim must sit relative to it"
        )
    });
    assert!(
        claim < spawn,
        "src/commands/utility.rs: `.claim()` is inside or after the          `spawn_blocking` call, so the cell is ordered by whichever build          reached the pool first rather than by which save reached disk first"
    );

    let persist = body.find("persist(settings)?").unwrap_or_else(|| {
        panic!(
            "no `persist(settings)?` in src/commands/utility.rs: the save no              longer runs before the reconfigure"
        )
    });
    assert!(
        persist < claim,
        "src/commands/utility.rs: the generation is claimed before the          settings are persisted, so the cell's order can still disagree with          the order the files were written in"
    );
}

/// `proxy::system_proxy_from_env` is a deliberate mirror of one reqwest
/// release. 0.11.27 applies `ALL_PROXY` last and lets it overwrite
/// `HTTP_PROXY`/`HTTPS_PROXY`; 0.12 reversed that. Our copy reproduces
/// 0.11.27 on purpose — restoring a scrubbed environment has to send the
/// user's traffic where their own configuration was already sending it, not
/// where a better rule would.
///
/// So a major bump is a behaviour change in that function, not a dependency
/// update, and must not land silently. This fails the suite when the pin moves,
/// which is the prompt to re-read `get_from_environment` in the new release and
/// update both the mirror and its doc comment.
#[test]
fn the_mirrored_reqwest_major_version_is_still_what_we_pin() {
    let toml = fs::read_to_string("Cargo.toml").expect("src-tauri/Cargo.toml");
    let line = toml
        .lines()
        .find(|l| l.trim_start().starts_with("reqwest"))
        .expect("no reqwest dependency in Cargo.toml");
    assert!(
        line.contains("version = \"0.11\"") || line.contains("reqwest = \"0.11\""),
        "reqwest is pinned as `{}`, but `proxy::system_proxy_from_env` mirrors \
         0.11.27's `get_from_environment` — including the `ALL_PROXY` \
         precedence 0.12 reversed. Re-read that function in the new release, \
         update the mirror and its doc comment, then update this guard.",
        line.trim()
    );
}
