// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(target_os = "linux")]
    {
        // WebKitGTK's DMA-BUF renderer is unreliable on the NVIDIA proprietary
        // driver: GBM buffer allocation fails (blank/corrupt page rendering) and
        // the GStreamer video path tears and stutters (WebKit Bugzilla #261874
        // and #260654, tauri-apps/tauri#9394). This affects BOTH X11 and Wayland
        // — the web process renders surfaceless, so the DMA-BUF renderer is used
        // regardless of session type. Fall back to shared-memory rendering
        // whenever an NVIDIA kernel module is loaded. Pre-set
        // WEBKIT_DISABLE_DMABUF_RENDERER to override.
        //
        // TODO: revisit when WebKitGTK resolves the NVIDIA DMA-BUF bug
        // (upstream #262607 is WONTFIX as of 2026).
        let already_overridden =
            std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_some();

        if !already_overridden {
            let nvidia_loaded = std::fs::read_to_string("/proc/modules")
                .map(|modules| {
                    modules.lines().any(|line| {
                        line.split_whitespace()
                            .next()
                            .map(|name| name == "nvidia" || name.starts_with("nvidia_"))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);

            if nvidia_loaded {
                eprintln!(
                    "[sone] NVIDIA detected; setting \
                     WEBKIT_DISABLE_DMABUF_RENDERER=1 to avoid WebKitGTK GBM \
                     allocation failure and video corruption. Pre-set the \
                     variable to override."
                );
                std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
            }
        }

        // Must happen here, while the process is still single-threaded: this is
        // the only sound place to mutate the environment, because glib/GTK
        // threads read it back via g_getenv once they exist. The audio worker
        // used to do it after spawning, which was unsound.
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
    tauri_app_lib::run()
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
    use super::gst_plugin_path_choice;

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
                Some("/tmp/.mount_sone"),
                None,
                &DIRS,
            ),
            Some("/app/lib/gstreamer-1.0".to_string())
        );
    }

    #[test]
    fn appdir_without_a_bundle_plugin_path_probes_nothing() {
        assert_eq!(
            gst_plugin_path_choice(None, Some("/tmp/.mount_sone"), None, &DIRS),
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
