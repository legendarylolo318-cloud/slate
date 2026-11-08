//! Per-output-device memory: the volume and the equalizer settings last used on each output
//! device, restored when that device comes back. Pure logic and the file format; the app
//! decides when to read and write (never unless the setting is on).
//!
//! At most [`MAX_DEVICES`] devices, the least recently used dropped first. The file is
//! read as hostile: a size cap, keys cut and cleaned, numbers clamped.

use serde::{Deserialize, Serialize};

/// Name of the encrypted state file.
pub const FILE: &str = "devices.json";
/// Most devices remembered.
pub const MAX_DEVICES: usize = 50;
/// Device keys are system names; longer ones are not real.
pub const MAX_KEY_CHARS: usize = 200;
/// A file bigger than this is not ours (50 devices of 10 bands is about 40 KB).
pub const MAX_FILE_BYTES: usize = 256 * 1024;

/// `pw:<PipeWire node name>`, `alsa:<exclusive device>` or `wasapi:<endpoint id>`.
pub fn key(kind: &str, id: &str) -> Option<String> {
    let id: String = id.chars().filter(|c| !c.is_control()).collect();
    let id = id.trim();
    (!id.is_empty()).then(|| {
        let k: String = format!("{kind}:{id}").chars().take(MAX_KEY_CHARS).collect();
        k
    })
}

/// What coming to a device with a remembered `entry` restores. `first` is the first device
/// seen since launch; with "start at half volume" on, that launch decides the volume.
pub fn volume_to_restore(entry: &Entry, first: bool, start_half_volume: bool) -> Option<f32> {
    entry.volume.filter(|_| !(first && start_half_volume))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eq: Option<crate::eq::Settings>,
}

/// Most recently used last.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceMemory {
    entries: Vec<Entry>,
}

impl DeviceMemory {
    /// Unreadable or malformed input is an empty store, never an error.
    pub fn parse(text: &str) -> Self {
        if text.len() > MAX_FILE_BYTES {
            return Self::default();
        }
        let raw: Self = serde_json::from_str(text).unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        let mut keep: Vec<Entry> = Vec::new();
        // The later entry of a repeated key wins.
        for mut e in raw.entries.into_iter().rev() {
            if e.key.len() > 4 * MAX_KEY_CHARS {
                continue;
            }
            let clean: String = e
                .key
                .chars()
                .filter(|c| !c.is_control())
                .take(MAX_KEY_CHARS)
                .collect();
            if clean.trim().is_empty() || !seen.insert(clean.clone()) {
                continue;
            }
            e.key = clean;
            e.volume = e
                .volume
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(0.0, 1.0));
            e.eq = e.eq.map(crate::eq::Settings::clamped);
            if e.volume.is_some() || e.eq.is_some() {
                keep.push(e);
            }
        }
        keep.reverse();
        let excess = keep.len().saturating_sub(MAX_DEVICES);
        keep.drain(..excess);
        Self { entries: keep }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == key)
    }

    /// Moves (or adds) `key` to the most-recently-used end and returns its index.
    fn touch(&mut self, key: &str) -> usize {
        match self.entries.iter().position(|e| e.key == key) {
            Some(i) => {
                let e = self.entries.remove(i);
                self.entries.push(e);
            }
            None => {
                if self.entries.len() >= MAX_DEVICES {
                    self.entries.remove(0);
                }
                self.entries.push(Entry {
                    key: key.to_string(),
                    volume: None,
                    eq: None,
                });
            }
        }
        self.entries.len() - 1
    }

    /// Remembers `volume` for `key`. True when that changed the store.
    pub fn set_volume(&mut self, key: &str, volume: f32) -> bool {
        if !volume.is_finite() {
            return false;
        }
        let volume = volume.clamp(0.0, 1.0);
        if self.get(key).and_then(|e| e.volume) == Some(volume) {
            return false;
        }
        let i = self.touch(key);
        self.entries[i].volume = Some(volume);
        true
    }

    /// Remembers the equalizer settings for `key`. True when that changed the store.
    pub fn set_eq(&mut self, key: &str, eq: &crate::eq::Settings) -> bool {
        let eq = eq.clone().clamped();
        if self.get(key).and_then(|e| e.eq.as_ref()) == Some(&eq) {
            return false;
        }
        let i = self.touch(key);
        self.entries[i].eq = Some(eq);
        true
    }

    /// Marks `key` as the most recently used (a device that came back).
    pub fn used(&mut self, key: &str) -> bool {
        if self.get(key).is_none() || self.entries.last().is_some_and(|e| e.key == key) {
            return false;
        }
        self.touch(key);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eq_with_preamp(db: f32) -> crate::eq::Settings {
        crate::eq::Settings {
            enabled: true,
            preamp_db: db,
            bands: Vec::new(),
        }
    }

    #[test]
    fn keys_are_namespaced_cleaned_and_bounded() {
        assert_eq!(
            key("pw", " alsa_output.usb \n").as_deref(),
            Some("pw:alsa_output.usb")
        );
        assert_eq!(key("pw", "  \u{7} "), None);
        assert_eq!(
            key("pw", &"x".repeat(1000)).unwrap().chars().count(),
            MAX_KEY_CHARS
        );
    }

    #[test]
    fn volume_and_eq_are_remembered_per_device() {
        let mut m = DeviceMemory::default();
        assert!(m.set_volume("pw:a", 0.3));
        assert!(!m.set_volume("pw:a", 0.3), "unchanged is not a change");
        assert!(m.set_volume("pw:b", 0.9));
        assert!(m.set_eq("pw:a", &eq_with_preamp(-3.0)));
        assert_eq!(m.get("pw:a").unwrap().volume, Some(0.3));
        assert_eq!(m.get("pw:b").unwrap().volume, Some(0.9));
        assert_eq!(m.get("pw:b").unwrap().eq, None);
        assert_eq!(m.get("pw:a").unwrap().eq.as_ref().unwrap().preamp_db, -3.0);
    }

    #[test]
    fn at_most_fifty_devices_the_least_recently_used_goes() {
        let mut m = DeviceMemory::default();
        for i in 0..60 {
            m.set_volume(&format!("pw:d{i}"), 0.5);
            if i == 10 {
                // d0 is used again, so d1 is the oldest.
                assert!(m.used("pw:d0"));
            }
        }
        assert_eq!(m.len(), MAX_DEVICES);
        assert!(m.get("pw:d0").is_some());
        assert!(m.get("pw:d1").is_none());
        assert!(m.get("pw:d59").is_some());
    }

    #[test]
    fn start_at_half_volume_wins_only_at_launch() {
        let e = Entry {
            key: "pw:a".into(),
            volume: Some(0.2),
            eq: None,
        };
        assert_eq!(
            volume_to_restore(&e, true, true),
            None,
            "launch with half volume on"
        );
        assert_eq!(
            volume_to_restore(&e, true, false),
            Some(0.2),
            "launch with it off"
        );
        assert_eq!(
            volume_to_restore(&e, false, true),
            Some(0.2),
            "a later device change"
        );
        let none = Entry {
            key: "pw:b".into(),
            volume: None,
            eq: None,
        };
        assert_eq!(volume_to_restore(&none, false, false), None);
    }

    #[test]
    fn round_trips_through_json() {
        let mut m = DeviceMemory::default();
        m.set_volume("alsa:hw:1,0", 0.25);
        m.set_eq("alsa:hw:1,0", &eq_with_preamp(2.0));
        assert_eq!(DeviceMemory::parse(&m.to_json()), m);
    }

    #[test]
    fn a_hostile_file_is_bounded_and_clamped() {
        assert!(DeviceMemory::parse("not json").is_empty());
        assert!(DeviceMemory::parse("{\"entries\":5}").is_empty());
        assert!(DeviceMemory::parse(&" ".repeat(MAX_FILE_BYTES + 1)).is_empty());
        let mut entries = Vec::new();
        for i in 0..500 {
            entries.push(format!("{{\"key\":\"pw:{i}\",\"volume\":7.5}}"));
        }
        entries.push("{\"key\":\"\",\"volume\":0.5}".into());
        entries.push("{\"key\":\"pw:nan\",\"volume\":null}".into());
        entries.push(format!(
            "{{\"key\":\"{}\",\"volume\":0.5}}",
            "k".repeat(500)
        ));
        entries.push("{\"key\":\"pw:ctl\\u0007x\",\"volume\":-3}".into());
        let m = DeviceMemory::parse(&format!("{{\"entries\":[{}]}}", entries.join(",")));
        assert_eq!(m.len(), MAX_DEVICES);
        assert!(m.get("pw:499").is_some_and(|e| e.volume == Some(1.0)));
        assert!(m.get("pw:ctlx").is_some_and(|e| e.volume == Some(0.0)));
        assert!(m.get("pw:nan").is_none() && m.get("").is_none());
        assert!(m
            .entries
            .iter()
            .all(|e| e.key.chars().count() <= MAX_KEY_CHARS));
    }

    #[test]
    fn eq_from_a_file_is_clamped_to_what_the_element_takes() {
        let m = DeviceMemory::parse(
            "{\"entries\":[{\"key\":\"pw:a\",\"eq\":{\"enabled\":true,\"preamp_db\":900,\"bands\":[]}}]}",
        );
        assert_eq!(
            m.get("pw:a").unwrap().eq.as_ref().unwrap().preamp_db,
            crate::eq::MAX_GAIN_DB
        );
    }

    #[test]
    fn a_repeated_key_keeps_the_later_entry() {
        let m = DeviceMemory::parse(
            "{\"entries\":[{\"key\":\"pw:a\",\"volume\":0.1},{\"key\":\"pw:a\",\"volume\":0.8}]}",
        );
        assert_eq!(m.len(), 1);
        assert_eq!(m.get("pw:a").unwrap().volume, Some(0.8));
    }
}
