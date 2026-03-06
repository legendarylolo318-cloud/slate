import type { Track, QueuedTrack } from "../types";

let counter = 0;

export function stampQid(track: Track): QueuedTrack {
  return { ...track, _qid: `q${++counter}` };
}

export function stampQids(tracks: Track[]): QueuedTrack[] {
  return tracks.map(stampQid);
}

export function ensureQid(track: Track): QueuedTrack {
  if ((track as QueuedTrack)._qid) return track as QueuedTrack;
  return stampQid(track);
}

export function advanceCounterPast(tracks: QueuedTrack[]): void {
  for (const t of tracks) {
    const m = t._qid?.match(/^q(\d+)$/);
    if (m) {
      const n = parseInt(m[1], 10);
      if (n > counter) counter = n;
    }
  }
}
