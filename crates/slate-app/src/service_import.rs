//! Import playlists from a Spotify or Apple Music export (Settings → General → Import).
//!
//! Reading and judging are pure (`slate_core::import_lib`). This file picks the files, matches
//! each distinct track through the same paced search the CSV import uses (ISRC first, then
//! title + artist + length), stops at once on Cancel or a rate limit, and shows a review —
//! matched, uncertain and not found, per playlist — before anything is created. Playlists
//! are then made unlisted with the existing playlist code.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use slate_core::commands::{library as lib_cmds, search};
use slate_core::import_lib::{self as il, Candidate, ImportPlaylist, Verdict, Want};

use crate::backend::Backend;
use crate::textutil::clean_line;

/// Pause between searches, as for the CSV import.
const PAUSE: Duration = Duration::from_millis(300);
/// Results asked of each search.
const RESULTS: u32 = 10;
/// Uncertain tracks listed in the review (the rest are counted).
const SHOWN_UNCERTAIN: usize = 25;

static RUNNING: AtomicBool = AtomicBool::new(false);

/// A review waiting for Create / Cancel.
struct Review {
    playlists: Vec<ImportPlaylist>,
    /// By `Want::key`.
    results: HashMap<String, Resolved>,
    /// The account it was matched for.
    epoch: u64,
}

#[derive(Clone)]
struct Resolved {
    verdict: Verdict,
    /// What was found, for the review of an uncertain match.
    shown: String,
}

static REVIEW: Mutex<Option<Review>> = Mutex::new(None);

fn review() -> std::sync::MutexGuard<'static, Option<Review>> {
    REVIEW.lock().unwrap_or_else(|e| e.into_inner())
}

/// Sign-out: a review waiting for Create is dropped.
pub fn forget() {
    *review() = None;
}

fn candidate(t: &slate_core::tidal_api::TidalTrack) -> Candidate {
    let artists: Vec<String> = match &t.artists {
        Some(a) if !a.is_empty() => a.iter().map(|a| a.name.clone()).collect(),
        _ => t.artist.iter().map(|a| a.name.clone()).collect(),
    };
    Candidate {
        id: t.id,
        title: t.title.clone(),
        artists,
        duration_secs: t.duration as f32,
        isrc: t.isrc.clone(),
    }
}

/// Reads every picked file into playlists, or says why one cannot be.
fn read_files(paths: &[std::path::PathBuf]) -> Result<Vec<ImportPlaylist>, String> {
    let mut all: Vec<ImportPlaylist> = Vec::new();
    for path in paths {
        let file = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("that file")
            .to_string();
        let len = std::fs::metadata(path)
            .map_err(|e| format!("Could not read {file}: {e}"))?
            .len();
        if len > il::MAX_FILE_BYTES {
            return Err(format!(
                "{file} is over {} MB, more than Slate reads. Split the export and import it in parts.",
                il::MAX_FILE_BYTES / (1024 * 1024)
            ));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("Could not read {file}: {e}"))?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Imported playlist")
            .to_string();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        let lists = match ext.as_str() {
            "json" => match il::spotify_kind(&text) {
                Some(il::Kind::SpotifyPlaylists) => il::parse_spotify_playlists(&text, &clean_line),
                Some(il::Kind::SpotifyLibrary) => {
                    il::parse_spotify_library(&text, &clean_line, "Liked Songs")
                }
                _ => {
                    Err("That JSON is not a Spotify YourLibrary.json or Playlist file.".to_string())
                }
            },
            "csv" | "txt" => il::parse_csv(&text, &clean_line, &stem),
            "xml" => il::parse_apple_library(&text, &clean_line),
            _ => Err(
                "Slate reads .json (Spotify), .csv (Exportify, Apple) and .xml (Apple Library)."
                    .to_string(),
            ),
        }
        .map_err(|e| format!("{file}: {e}"))?;
        all.extend(lists);
        // Caps over everything read so far, so a hostile pick of many files stops early.
        il::check_caps(&all)?;
    }
    if all.is_empty() {
        return Err("No playlists with tracks were found in those files.".into());
    }
    Ok(all)
}

/// The review dialog's text.
fn review_text(r: &Review) -> String {
    let mut out = String::new();
    let (mut tm, mut tu, mut tn) = (0, 0, 0);
    for p in &r.playlists {
        let verdicts: Vec<Verdict> = p
            .tracks
            .iter()
            .map(|w| {
                r.results
                    .get(&w.key())
                    .map_or(Verdict::NotFound, |x| x.verdict)
            })
            .collect();
        let c = il::count(&verdicts);
        tm += c.matched;
        tu += c.uncertain;
        tn += c.not_found;
        out.push_str(&format!(
            "{}: {} matched, {} uncertain, {} not found\n",
            p.name, c.matched, c.uncertain, c.not_found
        ));
    }
    out.push_str(&format!(
        "\nTotal: {tm} matched, {tu} uncertain, {tn} not found.\n"
    ));
    let mut seen = std::collections::HashSet::new();
    let mut listed = 0usize;
    let mut more = 0usize;
    for p in &r.playlists {
        for w in &p.tracks {
            let key = w.key();
            let Some(x) = r.results.get(&key) else {
                continue;
            };
            if !matches!(x.verdict, Verdict::Uncertain(_)) || !seen.insert(key) {
                continue;
            }
            if listed < SHOWN_UNCERTAIN {
                if listed == 0 {
                    out.push_str("\nUncertain (wanted → found):\n");
                }
                out.push_str(&format!("{} — {} → {}\n", w.title, w.artist, x.shown));
                listed += 1;
            } else {
                more += 1;
            }
        }
    }
    if more > 0 {
        out.push_str(&format!("…and {more} more uncertain\n"));
    }
    out
}

impl Backend {
    /// Settings → General → Import.
    pub(crate) fn service_import(&self) {
        if RUNNING.swap(true, Ordering::SeqCst) {
            self.notify("An import is already running");
            return;
        }
        let this = self.clone();
        self.rt.spawn(async move {
            let result = this.run_service_import().await;
            RUNNING.store(false, Ordering::SeqCst);
            this.ui(|w| w.set_notice("".into()));
            if let Err(message) = result {
                this.ui(move |w| w.set_error(message.into()));
            }
        });
    }

    async fn run_service_import(&self) -> Result<(), String> {
        // A sign-out while the picker is open, or during matching, ends the import: nothing
        // of it may reach the next account (#124).
        let epoch = self.account_epoch();
        let Some(files) = rfd::AsyncFileDialog::new()
            .set_title("Import from Spotify or Apple Music")
            .add_filter(
                "Spotify / Apple Music export",
                &["json", "csv", "xml", "txt"],
            )
            .pick_files()
            .await
        else {
            return Ok(());
        };
        let paths: Vec<_> = files.iter().map(|f| f.path().to_path_buf()).collect();
        let playlists = tokio::task::spawn_blocking(move || read_files(&paths))
            .await
            .map_err(|e| e.to_string())??;
        let state = self.state().ok_or("The core is still starting.")?;
        if !self.same_account(epoch) {
            return Ok(());
        }
        // `begin_bulk` has said why when another edit is running.
        let Some(job) = self.begin_bulk() else {
            return Ok(());
        };

        // Each distinct track once, in the order first seen.
        let mut wants: Vec<&Want> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        for w in playlists.iter().flat_map(|p| &p.tracks) {
            index.entry(w.key()).or_insert_with(|| {
                wants.push(w);
                wants.len() - 1
            });
        }
        let total = wants.len();
        let mut results: HashMap<String, Resolved> = HashMap::with_capacity(total);
        let mut searched = false;
        for (n, want) in wants.iter().enumerate() {
            if job.cancelled() || !self.same_account(epoch) {
                return Ok(());
            }
            let text = format!("Matching track {} of {total}…", n + 1);
            self.ui(move |w| w.set_notice(text.into()));
            let mut verdict = Verdict::NotFound;
            let mut found: Vec<Candidate> = Vec::new();
            // ISRC first; the title and artist search when that does not settle it.
            let mut queries: Vec<String> = Vec::new();
            if let Some(i) = &want.isrc {
                queries.push(i.clone());
            }
            queries.push(format!("{} {}", want.artist, want.title).trim().to_string());
            for q in queries {
                if searched {
                    tokio::time::sleep(PAUSE).await;
                }
                searched = true;
                match search::search_tidal(&state, q, RESULTS).await {
                    Ok(r) => {
                        found = r.tracks.iter().map(candidate).collect();
                        verdict = il::judge(want, &found);
                    }
                    Err(e) if e.is_rate_limited() => {
                        return Err(format!(
                            "TIDAL is limiting requests; matching stopped after {n} of {total} tracks and nothing was created. Try again in a few minutes."
                        ));
                    }
                    Err(e) => tracing::debug!("import: search failed: {}", e.log_safe()),
                }
                if matches!(verdict, Verdict::Matched(_)) {
                    break;
                }
            }
            let shown = match verdict {
                Verdict::Matched(id) | Verdict::Uncertain(id) => found
                    .iter()
                    .find(|c| c.id == id)
                    .map(|c| format!("{} — {}", c.title, c.artists.join(", ")))
                    .unwrap_or_default(),
                Verdict::NotFound => String::new(),
            };
            results.insert(want.key(), Resolved { verdict, shown });
        }
        drop(job);
        let r = Review {
            playlists,
            results,
            epoch,
        };
        let text = review_text(&r);
        {
            // Checked under the review lock, which `forget` takes at sign-out.
            let mut g = review();
            if !self.same_account(epoch) {
                return Ok(());
            }
            *g = Some(r);
        }
        self.ui(move |w| {
            w.set_dialog_text(text.into());
            w.set_dialog_target("".into());
            w.set_dialog_kind("import-review".into());
            w.set_dialog_open(true);
        });
        Ok(())
    }

    /// The review's Create buttons: `with_uncertain` adds the uncertain matches.
    pub(crate) fn service_import_create(&self, with_uncertain: bool) {
        let Some(r) = review().take() else { return };
        if !self.same_account(r.epoch) {
            return;
        }
        if RUNNING.swap(true, Ordering::SeqCst) {
            *review() = Some(r);
            self.notify("An import is already running");
            return;
        }
        let this = self.clone();
        self.rt.spawn(async move {
            let msg = this.create_imported(r, with_uncertain).await;
            RUNNING.store(false, Ordering::SeqCst);
            this.load_library();
            this.notify(&msg);
        });
    }

    async fn create_imported(&self, r: Review, with_uncertain: bool) -> String {
        let Some(state) = self.state() else {
            return "The core is still starting.".into();
        };
        let Some(job) = self.begin_bulk() else {
            return "Another playlist edit is still running.".into();
        };
        let (mut made, mut tracks, mut left_out) = (0usize, 0usize, 0usize);
        let total = r.playlists.len();
        for (n, p) in r.playlists.iter().enumerate() {
            if job.cancelled() || !self.same_account(r.epoch) {
                break;
            }
            let mut ids: Vec<u64> = Vec::new();
            for w in &p.tracks {
                match r.results.get(&w.key()).map(|x| x.verdict) {
                    Some(Verdict::Matched(id)) => ids.push(id),
                    Some(Verdict::Uncertain(id)) if with_uncertain => ids.push(id),
                    _ => left_out += 1,
                }
            }
            if ids.is_empty() {
                continue;
            }
            let text = format!("Creating playlist {} of {total}: {}", n + 1, p.name);
            self.ui(move |w| w.set_notice(text.into()));
            match lib_cmds::create_playlist_with_tracks_until(
                &state,
                p.name.clone(),
                "UNLISTED",
                &ids,
                &|| job.cancelled(),
            )
            .await
            {
                Ok((pl, added)) => {
                    self.note_created(&pl.uuid, &pl.title);
                    made += 1;
                    tracks += added;
                }
                Err(e) if e.is_rate_limited() => {
                    return format!(
                        "TIDAL is limiting requests; {made} of {total} playlists were created. Try again in a few minutes."
                    );
                }
                Err(e) => {
                    return format!(
                        "Could not create \"{}\": {}. {made} of {total} playlists were created.",
                        p.name,
                        e.log_safe()
                    );
                }
            }
            tokio::time::sleep(PAUSE).await;
        }
        let cancelled = job.cancelled();
        drop(job);
        format!(
            "{}Created {made} unlisted playlist{} with {tracks} track{}; {left_out} left out",
            if cancelled { "Cancelled. " } else { "" },
            if made == 1 { "" } else { "s" },
            if tracks == 1 { "" } else { "s" }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(title: &str, artist: &str) -> Want {
        Want {
            title: title.into(),
            artist: artist.into(),
            ..Want::default()
        }
    }

    #[test]
    fn the_review_counts_per_playlist_and_lists_uncertain_ones() {
        let a = w("Africa", "Toto");
        let b = w("Rosanna", "Toto");
        let c = w("Gone", "Nobody");
        let mut results = HashMap::new();
        results.insert(
            a.key(),
            Resolved {
                verdict: Verdict::Matched(1),
                shown: String::new(),
            },
        );
        results.insert(
            b.key(),
            Resolved {
                verdict: Verdict::Uncertain(2),
                shown: "Rosanna (Live) — Toto".into(),
            },
        );
        results.insert(
            c.key(),
            Resolved {
                verdict: Verdict::NotFound,
                shown: String::new(),
            },
        );
        let r = Review {
            epoch: 0,
            playlists: vec![
                ImportPlaylist {
                    name: "One".into(),
                    tracks: vec![a.clone(), b.clone()],
                },
                ImportPlaylist {
                    name: "Two".into(),
                    tracks: vec![a, c],
                },
            ],
            results,
        };
        let t = review_text(&r);
        assert!(
            t.contains("One: 1 matched, 1 uncertain, 0 not found"),
            "{t}"
        );
        assert!(
            t.contains("Two: 1 matched, 0 uncertain, 1 not found"),
            "{t}"
        );
        assert!(
            t.contains("Total: 2 matched, 1 uncertain, 1 not found."),
            "{t}"
        );
        assert!(t.contains("Rosanna — Toto → Rosanna (Live) — Toto"), "{t}");
    }

    #[test]
    fn a_long_uncertain_list_is_cut_and_counted() {
        let mut results = HashMap::new();
        let tracks: Vec<Want> = (0..40).map(|i| w(&format!("T{i}"), "A")).collect();
        for t in &tracks {
            results.insert(
                t.key(),
                Resolved {
                    verdict: Verdict::Uncertain(1),
                    shown: "x".into(),
                },
            );
        }
        let r = Review {
            epoch: 0,
            playlists: vec![ImportPlaylist {
                name: "P".into(),
                tracks,
            }],
            results,
        };
        let t = review_text(&r);
        assert!(t.contains("…and 15 more uncertain"), "{t}");
    }

    #[test]
    fn files_are_refused_with_sentences() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("x.json");
        std::fs::write(&bad, "{\"other\":1}").unwrap();
        assert!(read_files(&[bad]).unwrap_err().contains("not a Spotify"));
        let odd = dir.path().join("x.bin");
        std::fs::write(&odd, "x").unwrap();
        assert!(read_files(&[odd])
            .unwrap_err()
            .contains("Slate reads .json"));
        let csv = dir.path().join("Road trip.csv");
        std::fs::write(&csv, "Track Name,Artist Name(s)\nAfrica,Toto\n").unwrap();
        let p = read_files(&[csv]).unwrap();
        assert_eq!(p[0].name, "Road trip");
        assert!(read_files(&[dir.path().join("missing.csv")])
            .unwrap_err()
            .contains("Could not read"));
    }

    #[test]
    fn candidates_take_every_credited_artist() {
        let t: slate_core::tidal_api::TidalTrack = serde_json::from_value(serde_json::json!({
            "id": 5, "title": "S", "duration": 200,
            "artist": {"id": 1, "name": "A"},
            "artists": [{"id": 1, "name": "A"}, {"id": 2, "name": "B"}],
            "isrc": "USSM18200001"
        }))
        .unwrap();
        let c = candidate(&t);
        assert_eq!((c.id, c.artists.len(), c.duration_secs), (5, 2, 200.0));
    }
}
