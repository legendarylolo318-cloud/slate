//! Reduce motion: the saved choice (Off / On / Follow system) becomes `Motion.reduce`, which
//! every animation's duration reads. Off never asks the system anything.

use slint::ComponentHandle;

use crate::backend::Backend;
use crate::bootstrap::Motion;

/// Whether the desktop asks for less motion. Linux: GNOME's `enable-animations` switch (false
/// means reduce). Other desktops, and Windows, say nothing, so motion stays on.
pub fn system_reduces_motion() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "enable-animations"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .is_some_and(|o| parse_enable_animations(&String::from_utf8_lossy(&o.stdout)))
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// `gsettings get` prints `true` or `false`; animations being off means reduce.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_enable_animations(out: &str) -> bool {
    out.trim() == "false"
}

/// 0 off, 1 on, 2 follow the system.
pub fn resolve(mode: u8, system: impl FnOnce() -> bool) -> bool {
    match mode {
        1 => true,
        2 => system(),
        _ => false,
    }
}

impl Backend {
    pub(crate) fn apply_motion(&self, mode: u8) {
        let this = self.clone();
        // The system question runs a process: off the UI thread.
        self.rt.spawn_blocking(move || {
            let reduce = resolve(mode, system_reduces_motion);
            this.ui(move |w| w.global::<Motion>().set_reduce(reduce));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_resolve_and_off_never_asks_the_system() {
        assert!(!resolve(0, || panic!("off must not ask")));
        assert!(resolve(1, || panic!("on must not ask")));
        assert!(resolve(2, || true));
        assert!(!resolve(2, || false));
        assert!(!resolve(9, || true), "an unknown value is off");
    }

    #[test]
    fn gnome_animations_off_means_reduce() {
        assert!(parse_enable_animations("false\n"));
        assert!(!parse_enable_animations("true\n"));
        assert!(!parse_enable_animations(""));
    }
}
