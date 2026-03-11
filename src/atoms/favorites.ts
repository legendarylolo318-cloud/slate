import { atom } from "jotai";
import type { AlbumDetail, ArtistDetail, FavoriteMix } from "../types";

export const favoriteTrackIdsAtom = atom<Set<number>>(new Set<number>());
export const favoriteAlbumIdsAtom = atom<Set<number>>(new Set<number>());
export const favoritePlaylistUuidsAtom = atom<Set<string>>(new Set<string>());
export const followedArtistIdsAtom = atom<Set<number>>(new Set<number>());
export const favoriteMixIdsAtom = atom<Set<string>>(new Set<string>());
export const optimisticFavoriteAlbumsAtom = atom<AlbumDetail[]>([]);
export const optimisticFollowedArtistsAtom = atom<ArtistDetail[]>([]);
export const optimisticFavoriteMixesAtom = atom<FavoriteMix[]>([]);
