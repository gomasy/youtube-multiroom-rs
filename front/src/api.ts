import { t, getLang } from "./i18n";
import type { Playlist, Track, TracksPage } from "./types";

export const PER_PAGE = 10;

let apiToken = localStorage.getItem("api_token");

export function getToken(): string | null {
  return apiToken;
}

export function setToken(token: string) {
  apiToken = token;
  localStorage.setItem("api_token", token);
}

function authHeaders(): Record<string, string> {
  const h: Record<string, string> = { "Content-Type": "application/json" };
  if (apiToken) h["Authorization"] = `Bearer ${apiToken}`;
  // Advertise the browser locale so the server can localize API messages
  // (toasts, etc.) to match the UI language.
  h["X-App-Lang"] = getLang();
  return h;
}

export async function authFetch(
  url: string,
  options: RequestInit = {},
  onUnauthorized?: () => void,
): Promise<Response> {
  options.headers = { ...authHeaders(), ...(options.headers as Record<string, string>) };
  const res = await fetch(url, options);
  if (res.status === 401) {
    onUnauthorized?.();
    throw new Error(t("api.unauthorized"));
  }
  return res;
}

export async function fetchTracks(
  page: number,
  perPage: number,
  onUnauthorized?: () => void,
  token?: string,
  playlistId?: string | null,
): Promise<TracksPage> {
  let url = `/api/tracks?page=${page}&per_page=${perPage}`;
  if (playlistId) url += `&playlist=${encodeURIComponent(playlistId)}`;
  const res = await authFetch(
    url,
    token ? { headers: { Authorization: `Bearer ${token}` } } : {},
    onUnauthorized,
  );
  if (!res.ok) throw new Error(t("api.fetchTracksFailed"));
  return res.json();
}

export async function reorderTrack(
  trackId: string,
  newIndex: number,
  onUnauthorized?: () => void,
  playlistId?: string | null,
): Promise<void> {
  const res = await authFetch(
    "/api/tracks/reorder",
    {
      method: "POST",
      body: JSON.stringify({
        track_id: trackId,
        new_index: newIndex,
        playlist: playlistId ?? null,
      }),
    },
    onUnauthorized,
  );
  if (!res.ok) throw new Error(t("api.reorderFailed"));
}

export async function getStreamUrl(
  trackId: string,
  onUnauthorized?: () => void,
): Promise<string> {
  const res = await authFetch(
    `/api/audio/${encodeURIComponent(trackId)}/url`,
    {},
    onUnauthorized,
  );
  if (!res.ok) throw new Error(t("api.streamUrlFailed"));
  const data = await res.json();
  return data.url;
}

export async function createPlaylist(
  name: string,
  onUnauthorized?: () => void,
): Promise<Playlist> {
  const res = await authFetch(
    "/api/playlists",
    { method: "POST", body: JSON.stringify({ name }) },
    onUnauthorized,
  );
  if (!res.ok) throw new Error(t("api.createPlaylistFailed"));
  const data = await res.json();
  return data.playlist;
}

export async function deletePlaylist(
  playlistId: string,
  onUnauthorized?: () => void,
): Promise<void> {
  const res = await authFetch(
    `/api/playlists/${encodeURIComponent(playlistId)}`,
    { method: "DELETE" },
    onUnauthorized,
  );
  if (!res.ok) throw new Error(t("api.deletePlaylistFailed"));
}

export async function addToPlaylist(
  playlistId: string,
  trackId: string,
  onUnauthorized?: () => void,
): Promise<{ message?: string }> {
  const res = await authFetch(
    `/api/playlists/${encodeURIComponent(playlistId)}/tracks`,
    { method: "POST", body: JSON.stringify({ track_id: trackId }) },
    onUnauthorized,
  );
  if (!res.ok) throw new Error(t("api.addToPlaylistFailed"));
  return res.json();
}

export async function removeFromPlaylist(
  playlistId: string,
  trackId: string,
  onUnauthorized?: () => void,
): Promise<void> {
  const res = await authFetch(
    `/api/playlists/${encodeURIComponent(playlistId)}/tracks/${encodeURIComponent(trackId)}`,
    { method: "DELETE" },
    onUnauthorized,
  );
  if (!res.ok) throw new Error(t("api.removeFromPlaylistFailed"));
}

export async function searchYouTube(
  query: string,
  onUnauthorized?: () => void,
): Promise<Track[]> {
  const res = await authFetch(
    `/api/search?q=${encodeURIComponent(query)}`,
    {},
    onUnauthorized,
  );
  if (!res.ok) throw new Error(t("api.searchFailed"));
  const data = await res.json();
  return data.results ?? [];
}

export async function playTracks(
  trackId: string,
  deviceIds: string[],
  onUnauthorized?: () => void,
): Promise<{ message?: string }> {
  const res = await authFetch(
    "/api/play",
    {
      method: "POST",
      body: JSON.stringify({ track_id: trackId, device_ids: deviceIds }),
    },
    onUnauthorized,
  );
  if (!res.ok) throw new Error(t("api.playFailed"));
  return res.json();
}

export async function queueNext(
  trackId: string,
  deviceIds: string[],
  onUnauthorized?: () => void,
): Promise<{ message?: string }> {
  const res = await authFetch(
    "/api/queue",
    {
      method: "POST",
      body: JSON.stringify({ track_id: trackId, device_ids: deviceIds }),
    },
    onUnauthorized,
  );
  if (!res.ok) throw new Error(t("api.queueFailed"));
  return res.json();
}

export async function removeQueueItem(
  deviceId: string,
  entry: string,
  onUnauthorized?: () => void,
): Promise<void> {
  const res = await authFetch(
    `/api/devices/${encodeURIComponent(deviceId)}/queue/${encodeURIComponent(entry)}`,
    { method: "DELETE" },
    onUnauthorized,
  );
  if (!res.ok) throw new Error(t("api.removeQueueFailed"));
}

export async function clearQueue(
  deviceId: string,
  onUnauthorized?: () => void,
): Promise<void> {
  const res = await authFetch(
    `/api/devices/${encodeURIComponent(deviceId)}/queue`,
    { method: "DELETE" },
    onUnauthorized,
  );
  if (!res.ok) throw new Error(t("api.clearQueueFailed"));
}

export async function checkAuth(
  token?: string,
): Promise<{ authorized: boolean; data: TracksPage | null }> {
  let unauthorized = false;
  try {
    const data = await fetchTracks(1, PER_PAGE, () => {
      unauthorized = true;
    }, token);
    return { authorized: true, data };
  } catch {
    return { authorized: !unauthorized, data: null };
  }
}
