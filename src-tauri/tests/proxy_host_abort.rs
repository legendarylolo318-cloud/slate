//! A malformed proxy URI aborts souphttpsrc inside GLib
//! (`gst_soup_uri_to_string: code should not be reached`, SIGABRT / exit 134).
//! Validation must reject such hosts before any element sees them, so this
//! guards the boundary from outside the test process.

use std::process::Command;

use tauri_app_lib::proxy::{self, HostCaps, PlanError};
use tauri_app_lib::{ProxySettings, ProxyType};

/// Hosts that must never reach a GStreamer element, each paired with the
/// rejection `proxy::plan` owes it.
const ABORTING_HOSTS: &[(&str, PlanError)] = &[
    ("[::1]", PlanError::BracketedHost),
    ("1.2.3.4:9999", PlanError::EmbeddedPort),
    ("[[::1]]", PlanError::BracketedHost),
];

fn settings_for(host: &str) -> ProxySettings {
    ProxySettings {
        enabled: true,
        proxy_type: ProxyType::Http,
        host: host.to_string(),
        port: 8080,
        username: None,
        password: None,
    }
}

fn python_available() -> bool {
    Command::new("python3")
        .arg("-c")
        .arg("import gi; gi.require_version('Gst','1.0')")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn malformed_proxy_hosts_abort_the_element_and_so_must_be_rejected_upstream() {
    // The invariant. Runs everywhere, including CI machines that have the
    // GStreamer dev headers but no runtime plugins and no python3-gi.
    for (host, expected) in ABORTING_HOSTS {
        let plan = proxy::plan(&settings_for(host), &HostCaps::assume_all_present());
        assert_eq!(
            plan.err().as_ref(),
            Some(expected),
            "proxy::plan must reject {host:?} before it can reach an element",
        );
    }

    // The evidence for why. Best effort only: without python3, the gi
    // bindings, or a GStreamer runtime there is nothing to observe, and the
    // invariant above stands on its own.
    if !python_available() {
        eprintln!("skipping element probe: python3 gi/Gst unavailable");
        return;
    }

    for (host, _) in ABORTING_HOSTS {
        let script = format!(
            r#"
import gi, sys
gi.require_version('Gst','1.0')
from gi.repository import Gst
Gst.init(None)
src = Gst.ElementFactory.make('souphttpsrc','s')
src.set_property('location','http://127.0.0.1:1/x')
src.set_property('proxy','http://{host}:8080')
p = Gst.Pipeline.new('p'); sink = Gst.ElementFactory.make('fakesink','f')
p.add(src); p.add(sink); src.link(sink)
p.set_state(Gst.State.PLAYING)
p.get_bus().timed_pop_filtered(2*Gst.SECOND, Gst.MessageType.ERROR|Gst.MessageType.EOS)
p.set_state(Gst.State.NULL)
sys.exit(0)
"#
        );
        let Ok(out) = Command::new("python3").arg("-c").arg(&script).output() else {
            eprintln!("skipping element probe for {host:?}: python3 failed to spawn");
            continue;
        };

        // Documents the hazard: if this stops aborting upstream, the rejection
        // in proxy::validate_host may be relaxed — but not before. Observed
        // locally on GStreamer 1.26: `1.2.3.4:9999` and `[[::1]]` exit 134
        // (SIGABRT), while `[::1]` builds a well-formed URI and exits 0 — it is
        // rejected for being ambiguous with the port field, not for aborting.
        eprintln!("host {host:?} -> status {:?}", out.status);
    }
}
