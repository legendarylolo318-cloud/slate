//! "Copy queue link": `slate://queue/<base64url of {"v":1,"name":?,"ids":[…]}>`. Pure.
//!
//! A link is data from anywhere (a chat, a web page), so reading one is bounded in size, in
//! count and in nesting, takes only numbers for ids, and refuses with a sentence.
//!
//! The payload is plain JSON, or, when that is longer than [`COMPRESS_OVER`], a `z` byte
//! followed by its raw deflate stream (JSON starts with `{`, so the first byte says which).

use std::io::{Read, Write};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};

pub const PREFIX: &str = "slate://queue/";
/// Tracks a link carries; a longer queue is cut and the notice says so.
pub const MAX_TRACKS: usize = 500;
/// Longest link read at all (the single-instance socket reads the same).
pub const MAX_LINK_BYTES: usize = 16 * 1024;
/// Most that a compressed payload may expand to: bounds a zip-bomb link.
pub const MAX_JSON_BYTES: usize = 64 * 1024;
/// Names are cut here.
pub const MAX_NAME_CHARS: usize = 100;
/// JSON longer than this is deflated.
const COMPRESS_OVER: usize = 1024;
const VERSION: u32 = 1;

#[derive(Serialize)]
struct Out<'a> {
    v: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    ids: &'a [u64],
}

#[derive(Deserialize)]
struct In {
    #[serde(default)]
    v: serde_json::Value,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    ids: Vec<serde_json::Value>,
}

/// A queue link, made.
#[derive(Debug, PartialEq, Eq)]
pub struct Encoded {
    pub link: String,
    /// Tracks in the link (at most [`MAX_TRACKS`]).
    pub included: usize,
    /// Tracks the queue had.
    pub total: usize,
}

impl Encoded {
    pub fn was_cut(&self) -> bool {
        self.included < self.total
    }
}

/// A link, read.
#[derive(Debug, PartialEq, Eq)]
pub struct Decoded {
    pub name: Option<String>,
    pub ids: Vec<u64>,
}

/// Makes the link for `ids` (in order; duplicates kept, as the queue holds them).
pub fn encode(name: Option<&str>, ids: &[u64], clean: impl Fn(&str) -> String) -> Encoded {
    let included = ids.len().min(MAX_TRACKS);
    let name = name
        .map(|n| clip_chars(&clean(n), MAX_NAME_CHARS))
        .filter(|n| !n.is_empty());
    let json = serde_json::to_vec(&Out {
        v: VERSION,
        name: name.as_deref(),
        ids: &ids[..included],
    })
    .unwrap_or_default();
    let payload = if json.len() > COMPRESS_OVER {
        let mut z = flate2::write::DeflateEncoder::new(vec![b'z'], flate2::Compression::best());
        let packed = z.write_all(&json).ok().and_then(|()| z.finish().ok());
        match packed {
            Some(p) if p.len() < json.len() => p,
            _ => json,
        }
    } else {
        json
    };
    Encoded {
        link: format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(payload)),
        included,
        total: ids.len(),
    }
}

fn clip_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Reads a queue link. `clean` is applied to the name (the app's `clean_line`).
pub fn decode(link: &str, clean: impl Fn(&str) -> String) -> Result<Decoded, String> {
    let link = link.trim();
    if link.len() > MAX_LINK_BYTES {
        return Err(
            "This queue link is too long to be one of Slate's, so it was not opened.".into(),
        );
    }
    let b64 = link
        .strip_prefix(PREFIX)
        .ok_or("That is not a Slate queue link.")?;
    let bytes = URL_SAFE_NO_PAD
        .decode(b64.trim_end_matches('='))
        .map_err(|_| "This queue link is damaged (it is not valid base64).")?;
    let json = match bytes.first() {
        Some(b'{') => bytes,
        Some(b'z') => {
            let mut out = Vec::new();
            let mut r =
                flate2::read::DeflateDecoder::new(&bytes[1..]).take(MAX_JSON_BYTES as u64 + 1);
            r.read_to_end(&mut out)
                .map_err(|_| "This queue link is damaged (it does not unpack).")?;
            if out.len() > MAX_JSON_BYTES {
                return Err(
                    "This queue link unpacks to far more than a queue holds, so it was not opened."
                        .into(),
                );
            }
            out
        }
        _ => return Err("This queue link is damaged (unknown format).".into()),
    };
    if json.len() > MAX_JSON_BYTES {
        return Err("This queue link is too large to be a queue, so it was not opened.".into());
    }
    let parsed: In = serde_json::from_slice(&json)
        .map_err(|_| "This queue link is damaged (its list could not be read).")?;
    match parsed.v.as_u64() {
        Some(v) if v == u64::from(VERSION) => {}
        Some(v) => {
            return Err(format!(
                "This queue link is version {v}; this Slate reads version {VERSION}. Update Slate to open it."
            ))
        }
        None => return Err("This queue link has no version, so it was not opened.".into()),
    }
    if parsed.ids.len() > MAX_TRACKS {
        return Err(format!(
            "This queue link lists more than {MAX_TRACKS} tracks, which Slate never makes, so it was not opened."
        ));
    }
    let mut ids = Vec::with_capacity(parsed.ids.len());
    for v in &parsed.ids {
        match v.as_u64() {
            // 0 is no track; ids beyond 2^53 are not TIDAL ids.
            Some(n) if (1..=(1u64 << 53)).contains(&n) => ids.push(n),
            _ => return Err(
                "This queue link holds something that is not a track number, so it was not opened."
                    .into(),
            ),
        }
    }
    if ids.is_empty() {
        return Err("This queue link holds no tracks.".into());
    }
    let name = parsed
        .name
        .map(|n| clip_chars(&clean(&clip_chars(&n, MAX_NAME_CHARS * 4)), MAX_NAME_CHARS))
        .filter(|n| !n.is_empty());
    Ok(Decoded { name, ids })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> String {
        s.trim().to_string()
    }

    fn link_of(json: &str) -> String {
        format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(json))
    }

    #[test]
    fn a_short_queue_round_trips_as_plain_json() {
        let e = encode(Some("Road trip"), &[1, 22, 333], id);
        assert!(!e.was_cut() && e.included == 3);
        let raw = URL_SAFE_NO_PAD.decode(&e.link[PREFIX.len()..]).unwrap();
        assert_eq!(raw[0], b'{');
        assert_eq!(
            decode(&e.link, id).unwrap(),
            Decoded {
                name: Some("Road trip".into()),
                ids: vec![1, 22, 333]
            }
        );
    }

    #[test]
    fn a_long_queue_is_compressed_and_round_trips() {
        let ids: Vec<u64> = (0..500).map(|i| 100_000_000 + i * 7).collect();
        let e = encode(None, &ids, id);
        let raw = URL_SAFE_NO_PAD.decode(&e.link[PREFIX.len()..]).unwrap();
        assert_eq!(raw[0], b'z');
        assert!(e.link.len() < MAX_LINK_BYTES, "{}", e.link.len());
        assert_eq!(decode(&e.link, id).unwrap().ids, ids);
    }

    #[test]
    fn worst_case_ids_still_fit_the_link_cap() {
        // Incompressible-ish 16-digit ids, the most a link may carry.
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let ids: Vec<u64> = (0..500)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                1_000_000_000_000_000 + x % 8_000_000_000_000_000
            })
            .collect();
        let e = encode(Some(&"é".repeat(300)), &ids, id);
        assert!(e.link.len() <= MAX_LINK_BYTES, "{}", e.link.len());
        let d = decode(&e.link, id).unwrap();
        assert_eq!(d.ids, ids);
        assert_eq!(d.name.unwrap().chars().count(), MAX_NAME_CHARS);
    }

    #[test]
    fn more_than_500_tracks_are_cut_and_the_cut_is_reported() {
        let ids: Vec<u64> = (1..=620).collect();
        let e = encode(None, &ids, id);
        assert_eq!((e.included, e.total, e.was_cut()), (500, 620, true));
        assert_eq!(decode(&e.link, id).unwrap().ids.len(), 500);
    }

    #[test]
    fn hostile_links_are_refused_with_a_sentence() {
        let refused = |l: &str| decode(l, id).expect_err(l);
        assert!(refused("slate://party/abc").contains("not a Slate queue link"));
        assert!(refused("slate://queue/!!!").contains("damaged"));
        assert!(refused(&link_of("{\"v\":2,\"ids\":[1]}")).contains("version 2"));
        assert!(refused(&link_of("{\"ids\":[1]}")).contains("no version"));
        assert!(refused(&link_of("{\"v\":1,\"ids\":[\"1\"]}")).contains("not a track number"));
        assert!(refused(&link_of("{\"v\":1,\"ids\":[1.5]}")).contains("not a track number"));
        assert!(refused(&link_of("{\"v\":1,\"ids\":[-1]}")).contains("not a track number"));
        assert!(refused(&link_of("{\"v\":1,\"ids\":[0]}")).contains("not a track number"));
        assert!(refused(&link_of("{\"v\":1,\"ids\":[[1]]}")).contains("not a track number"));
        assert!(refused(&link_of("{\"v\":1,\"ids\":[1e300]}")).contains("not a track number"));
        assert!(refused(&link_of("{\"v\":1,\"ids\":[]}")).contains("no tracks"));
        assert!(refused(&link_of("[1,2]")).contains("damaged"));
        assert!(refused(&link_of("{\"v\":1,\"ids\":[1],\"x\":")).contains("damaged"));
        assert!(refused(&format!("{PREFIX}{}", "A".repeat(MAX_LINK_BYTES))).contains("too long"));
        let many: Vec<String> = (1..=501).map(|n| n.to_string()).collect();
        assert!(refused(&link_of(&format!(
            "{{\"v\":1,\"ids\":[{}]}}",
            many.join(",")
        )))
        .contains("more than 500"));
    }

    #[test]
    fn deep_nesting_and_a_deflate_bomb_are_refused_quickly() {
        // Deep enough to matter yet under the link cap, so the parser (not the length check)
        // sees it. (10,000 levels was refused as too long before parsing began.)
        let deep_x = format!(
            "{{\"v\":1,\"ids\":[1],\"x\":{}{}}}",
            "[".repeat(5_000),
            "]".repeat(5_000)
        );
        let l = link_of(&deep_x);
        assert!(l.len() <= MAX_LINK_BYTES);
        // An unknown field is skipped without recursion: read, and only the ids count.
        assert_eq!(decode(&l, id).unwrap().ids, vec![1]);
        let deep_ids = format!(
            "{{\"v\":1,\"ids\":{}1{}}}",
            "[".repeat(5_000),
            "]".repeat(5_000)
        );
        let l = link_of(&deep_ids);
        assert!(l.len() <= MAX_LINK_BYTES);
        assert!(decode(&l, id).is_err());
        // 10 MB of zeros deflates to ~10 KB.
        let mut z = flate2::write::DeflateEncoder::new(vec![b'z'], flate2::Compression::best());
        z.write_all(&vec![b' '; 10 * 1024 * 1024]).unwrap();
        let bomb = z.finish().unwrap();
        assert!(bomb.len() < MAX_LINK_BYTES);
        let l = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bomb));
        let t = std::time::Instant::now();
        assert!(decode(&l, id).unwrap_err().contains("far more"));
        assert!(t.elapsed().as_secs() < 2);
    }

    #[test]
    fn the_name_is_cleaned_and_bounded() {
        let l = link_of(&format!(
            "{{\"v\":1,\"name\":\"{}\",\"ids\":[5]}}",
            "ab".repeat(1000)
        ));
        let d = decode(&l, id).unwrap();
        assert_eq!(d.name.unwrap().chars().count(), MAX_NAME_CHARS);
        let l = link_of("{\"v\":1,\"name\":\"  \",\"ids\":[5]}");
        assert_eq!(decode(&l, id).unwrap().name, None);
    }

    #[test]
    fn trailing_padding_is_tolerated() {
        let e = encode(None, &[7], id);
        let padded = format!("{}==", e.link);
        assert_eq!(decode(&padded, id).unwrap().ids, vec![7]);
    }
}
