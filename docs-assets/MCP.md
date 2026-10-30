# Slate MCP server

Slate can expose a local [MCP](https://modelcontextprotocol.io) server so an AI agent (for example
Hermes) can search TIDAL, control playback and manage playlists. It is **off by default**; turn it on
in Settings → Integrations. It listens on `127.0.0.1` only, checks the `Host` header, and the URL
contains a secret token. When it is off nothing of it runs: no socket, no tasks, no log file.

Connect your client to the URL shown in Settings (`http://127.0.0.1:5577/<token>/mcp`, Streamable
HTTP). The server keeps a session per client, so it knows the client's name from the `initialize`
handshake and can push notifications.

## Using Slate with Hermes and other agents

* **Access token (optional).** Settings → Integrations → Access token → Generate. Clients must then
  send `Authorization: Bearer <token>`; requests and subscriptions without it get `401`. The token
  is stored in Slate's encrypted settings file. Regenerate or remove it at any time.
* **Read-only mode.** Hides every tool that changes anything and rejects calls to them.
* **Approval.** With "Ask before destructive agent actions" (on by default), a destructive call that
  carries `"confirm": true` shows *“<client> wants to delete playlist ‘X’ — Allow / Deny”* in Slate.
  The tool waits; no answer within 60 s is a Deny. One dialog at a time, queued. Without
  `"confirm": true` a destructive tool only describes what it would do and changes nothing.
* **Let an agent DJ.** When on, and a client is subscribed to `slate://events` for `queue_low`, an
  empty queue waits up to 10 s for the agent to add tracks before the usual autoplay radio takes
  over. With no subscriber (or the option off) behaviour is exactly as without it.
* **Durations.** `get_queue`, playlist, album and smart-playlist tools return `track_count` and
  `total_duration_seconds`; `get_queue` also returns `remaining_seconds`, `get_now_playing` returns
  `remaining_seconds` and `ends_at` (local time, `HH:MM`). `create_playlist_from_tracks`,
  `add_to_queue` and `play_next` take `target_duration_minutes` and stop once the total is within
  ±3 minutes of it, returning the final total.
* **Activity log and undo.** Every mutating call is logged (time, client, tool, summary, result;
  the last 500, in the config directory, written only while the server runs). Settings →
  Integrations → Agent activity shows them with **Undo** for queue, playlist (add, remove, reorder,
  create) and favourite changes. Undoing a creation asks before deleting; deleting a playlist cannot
  be undone.
* **Errors** say what to do (“No playlist named ‘Gym’; did you mean ‘Gym 2024’?”), not raw HTTP.
  List tools take `limit` (≤ 50, default 20) and `offset`.

## Tools

### Read-only

| Tool | Kind | What it does | Parameters | Example |
|---|---|---|---|---|
| `get_history` | read | Recently played tracks, newest first. | limit (≤50, default 20), offset | `{"limit":10}` |
| `search_albums` | read | Search TIDAL for albums. | query, limit, offset | `{"query":"discovery daft punk"}` |
| `search_artists` | read | Search TIDAL for artists. | query, limit, offset | `{"query":"kendrick"}` |
| `search_playlists` | read | Search TIDAL for playlists. | query, limit, offset | `{"query":"lofi beats"}` |
| `get_mixes` | read | The user's TIDAL mixes (daily mixes, radios). | limit, offset | `{"limit":10}` |
| `get_explore` | read | TIDAL's Explore page: genres and moods, with what each section holds. | limit (sections), offset | `{}` |
| `get_similar_artists` | read | Artists similar to an artist. | id (artist id), limit | `{"id":1566}` |
| `get_track_credits` | read | Who made a track (writers, producers, performers). | id (track id) | `{"id":123}` |
| `get_album_info` | read | Details of an album: artist, year, track count, total duration, quality, copyright. | id (album id) | `{"id":77646168}` |
| `get_artist_info` | read | Details and bio of an artist. | id (artist id) | `{"id":1566}` |
| `get_other_versions` | read | Other versions of a track (explicit/clean, remasters, live, Dolby Atmos…). | id (track id), limit | `{"id":123}` |
| `get_playlist_suggestions` | read | Tracks TIDAL suggests adding to a playlist (never ones already in it). | playlist_uuid or playlist_name, limit | `{"playlist_name":"Road trip","limit":10}` |
| `compare_playlists` | read | Compare two playlists: tracks only in A, only in B, in both (matched by ISRC, else track id). | a_uuid|a_name, b_uuid|b_name | `{"a_name":"Gym","b_name":"Run"}` |
| `match_tracks` | read | Turn ISRCs or "artist - title" strings into TIDAL track ids (one paced search each, at most 100). | items | `{"items":["Daft Punk - One More Time"]}` |
| `get_current_lyric_line` | read | The lyric line being sung right now, if the track has synced lyrics. | — | `{}` |
| `get_signal_path` | read | How the audio reaches the speakers: codec, bit depth, sample rate, exclusive / bit-perfect, crossfade, and whether the path is PRISTINE or MODIFIED. | — | `{}` |
| `get_listening_stats` | read | Top artists and tracks from Slate's local play log (kept only while the MCP server runs). | period (day|week|month|year|all), limit | `{"period":"week"}` |
| `get_smart_playlists` | read | The user's smart playlists (rule-based, local). | — | `{}` |
| `get_party_status` | read | Whether a listening party is active: role, guest count and the current track. Read-only. | — | `{}` |
| `search_tracks` | read | Search the Tidal catalog for tracks, albums, artists and playlists. | query, limit (≤50, default 10, per category) | `{"query":"daft punk discovery","limit":5}` |
| `get_track_radio` | read | Get tracks similar to a given track (Tidal's recommendation engine). | track_id, limit (≤50, default 10) | `{"track_id":123,"limit":10}` |
| `get_artist_top_tracks` | read | Get an artist's most-popular tracks. | artist_id, limit (≤50, default 10) | `{"artist_id":1566}` |
| `get_album_tracks` | read | Get the tracklist of an album with track_count and total_duration_seconds. | album_id | `{"album_id":77646168}` |
| `get_track_lyrics` | read | Get the lyrics of a track if available (plain text and optional synced lines). | track_id | `{"track_id":123}` |
| `get_favorite_tracks` | read | The user's favourite tracks, newest first. | limit (≤100, default 50), offset | `{"limit":25}` |
| `get_favorite_albums` | read | The user's favourite albums. | limit (≤100, default 50), offset | `{"limit":25}` |
| `get_favorite_artists` | read | The user's favourite artists. | limit (≤100, default 50), offset | `{"limit":25}` |
| `is_track_favorited` | read | Whether a track is in the user's favourites. | track_id | `{"track_id":123}` |
| `get_user_playlists` | read | List the playlists in the user's library, owned and followed, with uuid, title, track_count and total_duration_seconds. | limit (≤50, default 20), offset | `{"limit":20}` |
| `get_playlist_tracks` | read | Get the tracks of a playlist with track_count and total_duration_seconds. | playlist_uuid or playlist_name (case-insensitive), limit (≤50, default 20), offset | `{"playlist_name":"Road trip","limit":20}` |
| `get_now_playing` | read | Get the currently-playing track, position, duration, play state, remaining_seconds and ends_at (local time, HH:MM). Returns nowPlaying:null if nothing is playing. | — | `{}` |
| `get_queue` | read | Get the upcoming tracks in the play queue with track_count, total_duration_seconds and remaining_seconds (rest of the current track plus the queue). | limit (≤50, default 20), offset | `{"limit":10}` |

### Change things (easy to put back)

| Tool | Kind | What it does | Parameters | Example |
|---|---|---|---|---|
| `add_to_queue` | write | Append tracks to the end of the Next-up queue. | track_ids (array), target_duration_minutes (optional, adds ids in order until within ±3 min) | `{"track_ids":[123,456]}` |
| `play_next` | write | Insert tracks at the front of the Next-up queue so they play right after the current track. | track_ids | `{"track_ids":[123]}` |
| `move_queue_item` | write | Move one upcoming track to another position in the queue. | from, to (0-based) | `{"from":3,"to":0}` |
| `create_playlist_from_tracks` | write | Create a playlist in one call from track ids, ISRCs or "artist - title" strings. | name, items, target_duration_minutes (optional) | `{"name":"Focus","items":["Brian Eno - An Ending (Ascent)"],"target_duration_minutes":45}` |
| `play_smart_playlist` | write | Evaluate a smart playlist and play it. | id (from get_smart_playlists) | `{"id":"sp1"}` |
| `play_query` | write | Play what the user asked for in plain words ("the new Kendrick album", "that Daft Punk song from Tron", "lofi playlist"). Searches tracks, albums, artists and playlists, scores the matches and plays the best one when confidence ≥ 0.7 (or play_anyway=true); otherwise returns the alternatives without playing. | query, play_anyway, target_duration_minutes | `{"query":"lofi playlist"}` |
| `batch` | write | Run up to 20 tool calls in order in one request. Each call is checked exactly as if made alone (read-only mode, confirm, approval). | calls [{tool, arguments}], stop_on_error (default true) | `{"calls":[{"tool":"search_tracks","arguments":{"query":"halo"}},{"tool":"pause"}]}` |
| `favorite_track` | write | Add or remove a track from the favourites. | id (track id), action = "add" or "remove" | `{"id":123,"action":"add"}` |
| `favorite_album` | write | Add or remove an album from the favourites. | id (album id), action = "add" or "remove" | `{"id":77646168,"action":"add"}` |
| `favorite_artist` | write | Add or remove an artist from the favourites. | id (artist id), action = "add" or "remove" | `{"id":1566,"action":"add"}` |
| `play_tracks` | write | Play, queue or insert tracks. | track_ids, action = "play_now" (replace the queue and play), "queue" (append) or "play_next" (insert after the current track) | `{"track_ids":[1,2],"action":"queue"}` |
| `play_source` | write | Play a whole playlist, album, an artist's top tracks or a mix. | source_type ("playlist"|"album"|"artist"|"mix"), id | `{"source_type":"album","id":"77646168"}` |
| `shuffle_source` | write | Shuffle and play a whole playlist, album, artist or mix (turns shuffle on). | source_type, id | `{"source_type":"playlist","id":"abc"}` |
| `pause` | write | Pause playback. | — | `{}` |
| `resume` | write | Resume playback. | — | `{}` |
| `skip_next` | write | Skip to the next track. | — | `{}` |
| `skip_previous` | write | Go back to the previous track (or restart this one). | — | `{}` |
| `toggle_shuffle` | write | Toggle shuffle on or off. | — | `{}` |
| `seek_to` | write | Seek within the current track. | position_seconds | `{"position_seconds":90}` |
| `set_volume` | write | Set the volume from 0.0 (silent) to 1.0. | level | `{"level":0.4}` |
| `remove_from_queue` | write | Remove one track from the queue by track id. | track_id | `{"track_id":123}` |
| `set_repeat` | write | Set the repeat mode. | mode = "off", "all" or "one" | `{"mode":"all"}` |
| `create_playlist` | write | Create a playlist, optionally with initial tracks. | name, description (optional), track_ids (optional) | `{"name":"Focus","track_ids":[1,2,3]}` |
| `update_playlist` | write | Rename a playlist or change its description. | playlist_uuid, name (optional), description (optional) | `{"playlist_uuid":"abc","name":"New name"}` |
| `add_to_playlist` | write | Add tracks to an existing playlist. | playlist_uuid or playlist_name, track_ids | `{"playlist_name":"Gym","track_ids":[1,2]}` |

### Destructive (need `"confirm": true`)

| Tool | Kind | What it does | Parameters | Example |
|---|---|---|---|---|
| `dedupe_playlist` | destructive | Remove duplicate tracks (same ISRC, else same track) from a playlist, keeping the first. | playlist_uuid|playlist_name, dry_run (true = report only), confirm (must be true to remove) | `{"playlist_name":"Gym","dry_run":true}` |
| `reorder_playlist` | destructive | Move one track within a playlist. | playlist_uuid|playlist_name, from_index, to_index (0-based), confirm (must be true) | `{"playlist_name":"Gym","from_index":10,"to_index":0,"confirm":true}` |
| `clear_queue` | destructive | Clear the whole play queue. Destructive: needs confirm=true. | confirm | `{"confirm":true}` |
| `delete_playlist` | destructive | Permanently delete a playlist (cannot be undone). Destructive: needs confirm=true and may ask the user in the app. | playlist_uuid, confirm | `{"playlist_uuid":"abc","confirm":true}` |
| `remove_track_from_playlist` | destructive | Remove the track at a 0-based index from a playlist. Destructive: needs confirm=true. | playlist_uuid, index, confirm | `{"playlist_uuid":"abc","index":3,"confirm":true}` |

`batch` runs up to 20 calls in order and returns each result or error; every inner call is checked
exactly as if made alone (read-only mode, `confirm`, approval). `stop_on_error` defaults to `true`;
a `batch` cannot contain a `batch`.

`play_query` understands requests like “the new Kendrick album”, “that Daft Punk song from Tron” or
“lofi playlist”. It scores matches on title, artist, popularity and (for “new”) recency and returns
the chosen item, a `confidence` from 0 to 1 and up to three alternatives. It plays only when
`confidence ≥ 0.7` or `play_anyway` is true.

## Resources and events

| Resource | Contents |
|---|---|
| `slate://now-playing` | The playing track, position, `remaining_seconds`, `ends_at`. |
| `slate://queue` | Upcoming tracks, `total_duration_seconds`, `remaining_seconds`. |
| `slate://events` | The last 20 events (below). |

Subscribe with `resources/subscribe`; Slate sends `notifications/resources/updated` when one changes
(read the resource for the new state). Events, computed only while a client is subscribed:

| Event | When |
|---|---|
| `track_skipped` | A track was skipped before 30 s (before 50 % for tracks under 60 s); includes the position. |
| `track_replayed` | The same track played again in the session, or a seek back to the start after more than half. |
| `queue_low` | Two tracks remain in the queue (once per drop, not in repeat-all). |

## Limits

`get_listening_stats` is built from a play log kept only while the MCP server runs, so it covers the
time since you enabled it. Smart playlists are evaluated by the app when asked.
