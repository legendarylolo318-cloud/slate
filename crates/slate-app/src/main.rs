// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// No panicking shortcuts in production code; tests may use them freely.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

mod backend;
mod bootstrap;
// Only Linux has OS media controls and a tray so far (SMTC/tray on Windows are tracked issues).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod control;
mod deeplink;
mod favorites;
mod images;
mod imgcache;
mod ipc;
mod library;
mod mini;
mod models;
mod pages;
mod panels;
mod pkce;
mod platform;
mod profile;
mod quality;
mod selftest;
mod settings;
mod signal;
mod themes;
mod uiprefs;
mod video;

fn main() -> anyhow::Result<()> {
    // Environment surgery must happen before any thread exists: `set_var` and
    // `remove_var` are unsound once other threads may read the environment.
    #[cfg(target_os = "linux")]
    scrub_environment();
    #[cfg(windows)]
    point_gstreamer_at_bundle();
    if std::env::args().any(|a| a == "--self-test") {
        std::process::exit(selftest::run());
    }
    apply_ui_scale();

    // First argument that is a link, if any (`tidal://…` from the OS handler).
    let link = std::env::args()
        .skip(1)
        .find(|a| deeplink::parse(a).is_some());
    // A renderer-fallback relaunch is our own child: the parent still holds the
    // socket, so claiming it here would forward to the parent and quit.
    let is_relaunch = std::env::var_os("SLATE_RENDERER_INDEX").is_some();
    let incoming = if is_relaunch {
        None
    } else {
        match ipc::claim(link.as_deref()) {
            ipc::Claim::Forwarded => return Ok(()),
            ipc::Claim::Primary(rx) => Some(rx),
            ipc::Claim::Unavailable => None,
        }
    };
    bootstrap::run(incoming, link)
}

/// A bundled GStreamer lives next to the executable (`gst-plugins/`); use it
/// exclusively so a system-wide install cannot mix in incompatible plugins.
/// The scanner sits beside `slate.exe` so Windows finds the GStreamer DLLs it
/// links against; GIO's TLS module (HTTPS for souphttpsrc) is in `gio/modules`.
#[cfg(windows)]
fn point_gstreamer_at_bundle() {
    let Some(base) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
    else {
        return;
    };
    let plugins = base.join("gst-plugins");
    if plugins.is_dir() {
        std::env::set_var("GST_PLUGIN_PATH", &plugins);
        std::env::set_var("GST_PLUGIN_SYSTEM_PATH", &plugins);
        std::env::set_var("GST_PLUGIN_SCANNER", base.join("gst-plugin-scanner.exe"));
        if let Some(cache) = dirs_cache() {
            std::env::set_var("GST_REGISTRY", cache);
        }
    }
    let gio = base.join("gio").join("modules");
    if gio.is_dir() {
        std::env::set_var("GIO_MODULE_DIR", gio);
    }
}

#[cfg(windows)]
fn dirs_cache() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    let dir = std::path::PathBuf::from(base).join("slate");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("gst-registry.bin"))
}

/// The saved interface size reaches Slint through its scale-factor override,
/// which is read at startup. A value already in the environment wins.
fn apply_ui_scale() {
    if std::env::var_os("SLINT_SCALE_FACTOR").is_some() {
        return;
    }
    if let Some(scale) = uiprefs::UiPrefs::load().scale_factor() {
        std::env::set_var("SLINT_SCALE_FACTOR", scale.to_string());
    }
}

#[cfg(target_os = "linux")]
fn scrub_environment() {
    let sidecar =
        slate_core::config_dir_for_env().and_then(|d| slate_core::proxy::read_sidecar(&d));

    {
        let present = slate_core::proxy::SCRUBBED_PROXY_ENV_VARS
            .into_iter()
            .any(|v| std::env::var_os(v).is_some_and(|s| !s.is_empty()));
        slate_core::proxy::remember_launch_bypass(should_record_launch_bypass(
            present,
            sidecar.clone(),
        ));
    }

    if should_scrub_proxy_env(sidecar) {
        let captured: Vec<(String, String)> = slate_core::proxy::PROXY_ENV_VARS
            .into_iter()
            .filter_map(|v| std::env::var(v).ok().map(|value| (v.to_string(), value)))
            .collect();
        slate_core::proxy::remember_scrubbed_env(captured);

        for v in slate_core::proxy::SCRUBBED_PROXY_ENV_VARS {
            std::env::remove_var(v);
        }
    }

    {
        let plugin_path_1_0 = std::env::var("GST_PLUGIN_PATH_1_0").ok();
        let appdir = std::env::var("APPDIR").ok();
        let plugin_path = std::env::var("GST_PLUGIN_PATH").ok();
        let existing_dirs: Vec<&str> = GST_PLUGIN_DIR_CANDIDATES
            .into_iter()
            .filter(|dir| std::path::Path::new(dir).is_dir())
            .collect();
        if let Some(chosen) = gst_plugin_path_choice(
            plugin_path_1_0.as_deref(),
            appdir.as_deref(),
            plugin_path.as_deref(),
            &existing_dirs,
        ) {
            std::env::set_var("GST_PLUGIN_PATH", chosen);
        }
    }
}

#[cfg(target_os = "linux")]
fn should_scrub_proxy_env(sidecar: Option<(bool, String)>) -> bool {
    matches!(sidecar, Some((true, _)))
}

/// Whether the launch-time bypass list is recorded as still in force, given
/// whether one was present in the environment and whatever the launch sidecar
/// said.
///
/// Pure for the same reason as `should_scrub_proxy_env`, and needed more: the
/// gate is load-bearing in *both* directions and is one token from either
/// failure. Dropping the scrub term refuses every proxied tier, for the whole
/// session, for a user who launched with the proxy on and `no_proxy` exported —
/// a launch that scrubbed has no ambient bypass list left to defeat anything.
/// Dropping the whole call reopens the containment hole: `curlhttpsrc` reads
/// `no_proxy` when the element is constructed and forwards it as
/// `CURLOPT_NOPROXY`, which beats the `proxy` property, so a bypass list that
/// survived startup silently sends audio out on the user's real address.
#[cfg(target_os = "linux")]
fn should_record_launch_bypass(present: bool, sidecar: Option<(bool, String)>) -> bool {
    present && !should_scrub_proxy_env(sidecar)
}

/// System GStreamer plugin directories, probed in order, and only when the
/// process is not running from a bundle.
#[cfg(target_os = "linux")]
const GST_PLUGIN_DIR_CANDIDATES: [&str; 3] = [
    "/usr/lib/x86_64-linux-gnu/gstreamer-1.0",
    "/usr/lib64/gstreamer-1.0",
    "/usr/lib/gstreamer-1.0",
];

/// Decide what `GST_PLUGIN_PATH` should become, or `None` to leave it alone.
///
/// Pure so the precedence is testable; the caller does the filesystem probing
/// and passes only the candidate directories that exist, in preference order.
///
/// The rules are the ones the audio worker used before this moved to `main`,
/// and the order matters:
///
/// 1. Running from a bundle (`GST_PLUGIN_PATH_1_0` or `APPDIR` present): the
///    bundle wins. `GST_PLUGIN_PATH_1_0` **overwrites** an inherited
///    `GST_PLUGIN_PATH`, because a host value leaking into an AppImage points
///    at the host's plugins, which are the wrong ABI.
/// 2. `APPDIR` set but `GST_PLUGIN_PATH_1_0` absent: do nothing at all. A
///    bundle that did not export a plugin path is not asking to be pointed at
///    the host's system directories, so no probing happens.
/// 3. Otherwise, probe the system directories, but only if `GST_PLUGIN_PATH` is
///    not already set.
#[cfg(target_os = "linux")]
fn gst_plugin_path_choice(
    plugin_path_1_0: Option<&str>,
    appdir: Option<&str>,
    plugin_path: Option<&str>,
    existing_dirs: &[&str],
) -> Option<String> {
    if plugin_path_1_0.is_some() || appdir.is_some() {
        return plugin_path_1_0.map(str::to_string);
    }
    if plugin_path.is_some() {
        return None;
    }
    existing_dirs.first().map(|dir| dir.to_string())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{gst_plugin_path_choice, should_record_launch_bypass, should_scrub_proxy_env};

    #[test]
    fn a_recorded_enabled_proxy_scrubs_whatever_its_type() {
        assert!(should_scrub_proxy_env(Some((true, "http".into()))));
        assert!(should_scrub_proxy_env(Some((true, "socks5".into()))));
    }

    /// The regression this task must not introduce: the proxy toggle off means
    /// the system's own configuration applies, so an exported `http_proxy` has
    /// to survive startup untouched.
    #[test]
    fn the_proxy_toggle_off_leaves_the_environment_alone() {
        assert!(!should_scrub_proxy_env(Some((false, "http".into()))));
        assert!(!should_scrub_proxy_env(Some((false, "socks5".into()))));
    }

    /// No sidecar at all — every launch before this feature shipped, and every
    /// first launch after it.
    #[test]
    fn an_unknown_mode_leaves_the_environment_alone() {
        assert!(!should_scrub_proxy_env(None));
    }

    /// The whole truth table, because each row fails a different way and the
    /// two `false` rows fail in opposite directions.
    ///
    /// Row `(present, proxying)`: the launch scrubbed the bypass list, so
    /// nothing is left to defeat the proxy. Recording `true` here refuses
    /// every proxied tier for a session that is perfectly fine, and restarting
    /// reproduces it exactly — the failure that pins the `!should_scrub` term.
    #[test]
    fn a_launch_that_scrubbed_has_no_surviving_bypass_list() {
        assert!(!should_record_launch_bypass(
            true,
            Some((true, "http".into()))
        ));
        assert!(!should_record_launch_bypass(
            true,
            Some((true, "socks5".into()))
        ));
    }

    /// Row `(present, not proxying)`: nothing scrubbed, so the bypass list is
    /// still in the environment and will beat the `proxy` property the moment
    /// the user turns the proxy on. This is the containment hole, and the row
    /// that pins the call existing at all.
    #[test]
    fn a_bypass_list_that_survived_an_unscrubbed_launch_is_recorded() {
        assert!(should_record_launch_bypass(true, None));
        assert!(should_record_launch_bypass(
            true,
            Some((false, "http".into()))
        ));
    }

    /// Row `(absent, _)`: no bypass list at launch, nothing to record, whatever
    /// the sidecar said. Recording `true` on either of these refuses proxied
    /// playback for every user who never exported `no_proxy` at all.
    #[test]
    fn no_bypass_list_at_launch_records_nothing_either_way() {
        assert!(!should_record_launch_bypass(false, None));
        assert!(!should_record_launch_bypass(
            false,
            Some((false, "http".into()))
        ));
        assert!(!should_record_launch_bypass(
            false,
            Some((true, "http".into()))
        ));
    }

    const DIRS: [&str; 2] = ["/usr/lib64/gstreamer-1.0", "/usr/lib/gstreamer-1.0"];

    #[test]
    fn bundle_plugin_path_overwrites_an_inherited_one() {
        assert_eq!(
            gst_plugin_path_choice(
                Some("/app/lib/gstreamer-1.0"),
                None,
                Some("/usr/lib/gstreamer-1.0"),
                &DIRS,
            ),
            Some("/app/lib/gstreamer-1.0".to_string()),
            "a host GST_PLUGIN_PATH leaking into a bundle must not win"
        );
    }

    /// The ordinary AppImage layout: AppRun exports GST_PLUGIN_PATH_1_0 and the
    /// host has no GST_PLUGIN_PATH at all. Every other bundle case here passes a
    /// *set* plugin_path, so without this one an implementation that only
    /// honours _1_0 when something is already set passes the whole suite while
    /// leaving the most common bundle with no plugin path.
    #[test]
    fn a_bundle_plugin_path_is_used_when_nothing_was_inherited() {
        assert_eq!(
            gst_plugin_path_choice(Some("/app/lib/gstreamer-1.0"), None, None, &DIRS),
            Some("/app/lib/gstreamer-1.0".to_string()),
            "the canonical AppImage layout must still get the bundle's plugins"
        );
    }

    /// Same, with APPDIR also exported, which is what AppRun actually does.
    #[test]
    fn a_bundle_plugin_path_wins_with_appdir_present_and_nothing_inherited() {
        assert_eq!(
            gst_plugin_path_choice(
                Some("/app/lib/gstreamer-1.0"),
                Some("/tmp/.mount_slate"),
                None,
                &DIRS,
            ),
            Some("/app/lib/gstreamer-1.0".to_string())
        );
    }

    /// Row `(_1_0 set, APPDIR set, GST_PLUGIN_PATH set)`: the bundle still wins.
    #[test]
    fn a_bundle_plugin_path_overwrites_an_inherited_one_with_appdir_present() {
        assert_eq!(
            gst_plugin_path_choice(
                Some("/app/lib/gstreamer-1.0"),
                Some("/tmp/.mount_slate"),
                Some("/usr/lib/gstreamer-1.0"),
                &DIRS,
            ),
            Some("/app/lib/gstreamer-1.0".to_string())
        );
    }

    /// Row `(_1_0 unset, APPDIR set, GST_PLUGIN_PATH set)`: still a bundle, so
    /// still no probe. Without this, a mutant that falls through to the system
    /// directories on this row points an AppImage at host plugins undetected.
    #[test]
    fn appdir_with_an_inherited_path_probes_nothing_either() {
        assert_eq!(
            gst_plugin_path_choice(
                None,
                Some("/tmp/.mount_slate"),
                Some("/usr/lib/gstreamer-1.0"),
                &DIRS,
            ),
            None,
            "a bundle must never be pointed at the host's system plugin dirs"
        );
    }

    #[test]
    fn appdir_without_a_bundle_plugin_path_probes_nothing() {
        assert_eq!(
            gst_plugin_path_choice(None, Some("/tmp/.mount_slate"), None, &DIRS),
            None,
            "an AppImage must not be pointed at the host's system plugin dirs"
        );
    }

    #[test]
    fn outside_a_bundle_an_unset_path_takes_the_first_existing_dir() {
        assert_eq!(
            gst_plugin_path_choice(None, None, None, &DIRS),
            Some("/usr/lib64/gstreamer-1.0".to_string())
        );
    }

    #[test]
    fn outside_a_bundle_an_existing_path_is_left_alone() {
        assert_eq!(
            gst_plugin_path_choice(None, None, Some("/opt/gst"), &DIRS),
            None
        );
    }

    #[test]
    fn outside_a_bundle_with_no_existing_dirs_nothing_is_set() {
        assert_eq!(gst_plugin_path_choice(None, None, None, &[]), None);
    }
}

#[cfg(test)]
mod shortcut_overlay_tests {
    /// Every key the window's FocusScope handles must appear in the overlay.
    #[test]
    fn shortcut_overlay_lists_every_bound_key() {
        let app = include_str!("../ui/app.slint");
        let overlay = include_str!("../ui/shortcuts.slint");
        let start = app.find("key-pressed(e)").unwrap();
        let handler = &app[start..start + app[start..].find("reject").unwrap()];
        let label = |key: &str| -> String {
            match key {
                " " => "Space".into(),
                "Key.RightArrow" | "Key.LeftArrow" => "←".into(),
                "Key.Escape" => "Esc".into(),
                k => k.to_uppercase(),
            }
        };
        let mut checked = 0;
        for part in handler.split("e.text == ").skip(1) {
            let key = if let Some(rest) = part.strip_prefix('"') {
                &rest[..rest.find('"').unwrap()]
            } else {
                &part[..part.find([')', ' ']).unwrap()]
            };
            let want = format!("keys: \"{}", label(key));
            let found = overlay.contains(&want)
                || overlay.contains(&format!("{} / ", label(key)))
                || overlay.contains(&format!("+ {}", label(key)));
            assert!(found, "key {key:?} is bound but not in shortcuts.slint");
            checked += 1;
        }
        assert!(checked >= 12, "parsed only {checked} bindings");
    }
}
