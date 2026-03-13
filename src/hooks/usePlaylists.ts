import { useCallback } from "react";
import { useAtom, useAtomValue, useSetAtom } from "jotai";
import { invoke } from "@tauri-apps/api/core";
import {
  userPlaylistsAtom,
  deletedPlaylistIdsAtom,
} from "../atoms/playlists";
import { authTokensAtom } from "../atoms/auth";
import { invalidateCache, getPlaylistFolders, normalizePlaylistFolders } from "../api/tidal";
import type { Playlist, PlaylistOrFolder } from "../types";

export function usePlaylists() {
  const [userPlaylists, setUserPlaylists] = useAtom(userPlaylistsAtom);
  const setDeletedPlaylistIds = useSetAtom(deletedPlaylistIdsAtom);
  const authTokens = useAtomValue(authTokensAtom);

  const createPlaylist = useCallback(
    async (title: string, description: string = ""): Promise<Playlist> => {
      if (!authTokens?.user_id) throw new Error("Not authenticated");
      try {
        const playlist = await invoke<Playlist>("create_playlist", {
          userId: authTokens.user_id,
          title,
          description,
        });
        setUserPlaylists((prev) => [playlist, ...prev]);
        invalidateCache("user-playlists");
        return playlist;
      } catch (error: any) {
        console.error("Failed to create playlist:", error);
        throw error;
      }
    },
    [authTokens?.user_id, setUserPlaylists],
  );

  // Background re-fetch user playlists to pick up server-side changes (image, exact count)
  const refreshUserPlaylists = useCallback(() => {
    getPlaylistFolders("root", 0, 50)
      .then((res) => {
        const normalized = normalizePlaylistFolders(res);
        const playlists = normalized.items
          .filter((i): i is Extract<PlaylistOrFolder, { kind: "playlist" }> => i.kind === "playlist")
          .map((i) => i.data);
        if (playlists.length) setUserPlaylists(playlists);
      })
      .catch(() => {});
  }, [setUserPlaylists]);

  const updatePlaylistTrackCount = useCallback(
    (playlistId: string, delta: number) => {
      setUserPlaylists((prev) =>
        prev.map((p) =>
          p.uuid === playlistId
            ? {
                ...p,
                numberOfTracks: Math.max(0, (p.numberOfTracks ?? 0) + delta),
              }
            : p,
        ),
      );
      invalidateCache("user-playlists");
    },
    [setUserPlaylists],
  );

  const addTrackToPlaylist = useCallback(
    async (playlistId: string, trackId: number): Promise<void> => {
      try {
        await invoke("add_track_to_playlist", {
          playlistId,
          trackId: trackId,
        });
        updatePlaylistTrackCount(playlistId, 1);
        invalidateCache(`playlist:${playlistId}`);
        invalidateCache(`playlist-page:${playlistId}`);
        refreshUserPlaylists();
      } catch (error: any) {
        console.error("Failed to add track to playlist:", error);
        throw error;
      }
    },
    [updatePlaylistTrackCount, refreshUserPlaylists],
  );

  const removeTrackFromPlaylist = useCallback(
    async (playlistId: string, index: number): Promise<void> => {
      try {
        await invoke("remove_track_from_playlist", {
          playlistId,
          index,
        });
        updatePlaylistTrackCount(playlistId, -1);
        invalidateCache(`playlist:${playlistId}`);
        invalidateCache(`playlist-page:${playlistId}`);
        refreshUserPlaylists();
      } catch (error: any) {
        console.error("Failed to remove track from playlist:", error);
        throw error;
      }
    },
    [updatePlaylistTrackCount, refreshUserPlaylists],
  );

  const deletePlaylist = useCallback(
    async (playlistId: string): Promise<void> => {
      if (!authTokens?.user_id) throw new Error("Not authenticated");
      try {
        await invoke("delete_playlist", {
          userId: authTokens.user_id,
          playlistId,
        });
        setUserPlaylists((prev) => prev.filter((p) => p.uuid !== playlistId));
        setDeletedPlaylistIds((prev: Set<string>) =>
          new Set(prev).add(playlistId),
        );
        invalidateCache(`playlist:${playlistId}`);
        invalidateCache(`playlist-page:${playlistId}`);
        invalidateCache("user-playlists");
      } catch (error: any) {
        console.error("Failed to delete playlist:", error);
        throw error;
      }
    },
    [
      authTokens?.user_id,
      setUserPlaylists,
      setDeletedPlaylistIds,
    ],
  );

  const addTracksToPlaylist = useCallback(
    async (playlistId: string, trackIds: number[]): Promise<void> => {
      try {
        await invoke("add_tracks_to_playlist", {
          playlistId,
          trackIds,
        });
        updatePlaylistTrackCount(playlistId, trackIds.length);
        invalidateCache(`playlist:${playlistId}`);
        invalidateCache(`playlist-page:${playlistId}`);
        refreshUserPlaylists();
      } catch (error: any) {
        console.error("Failed to add tracks to playlist:", error);
        throw error;
      }
    },
    [updatePlaylistTrackCount, refreshUserPlaylists],
  );

  return {
    userPlaylists,
    createPlaylist,
    deletePlaylist,
    addTrackToPlaylist,
    removeTrackFromPlaylist,
    addTracksToPlaylist,
  };
}
