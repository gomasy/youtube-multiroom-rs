import { t, lang } from "./i18n";
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
  h["X-App-Lang"] = lang;
  return h;
}

async function authFetch(
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

/// Perform an authFetch and throw t(errorKey) on a non-OK response.
export async function authOk(
  url: string,
  errorKey: string,
  options: RequestInit = {},
  onUnauthorized?: () => void,
): Promise<Response> {
  const res = await authFetch(url, options, onUnauthorized);
  if (!res.ok) throw new Error(t(errorKey));
  return res;
}

/// authOk that parses the response body as JSON.
async function authJson<T>(
  url: string,
  errorKey: string,
  options: RequestInit = {},
  onUnauthorized?: () => void,
): Promise<T> {
  return (await authOk(url, errorKey, options, onUnauthorized)).json();
}

export async function fetchTracks(
  page: number,
  perPage: number,
  onUnauthorized?: () => void,
  token?: string,
  playlistId?: string | null,
  filter?: string,
): Promise<TracksPage> {
  let url = `/api/tracks?page=${page}&per_page=${perPage}`;
  if (playlistId) url += `&playlist=${encodeURIComponent(playlistId)}`;
  if (filter) url += `&q=${encodeURIComponent(filter)}`;
  return authJson(
    url,
    "api.fetchTracksFailed",
    token ? { headers: { Authorization: `Bearer ${token}` } } : {},
    onUnauthorized,
  );
}

export async function reorderTrack(
  trackId: string,
  newIndex: number,
  onUnauthorized?: () => void,
  playlistId?: string | null,
): Promise<void> {
  await authOk(
    "/api/tracks/reorder",
    "api.reorderFailed",
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
}

export async function getStreamUrl(
  trackId: string,
  onUnauthorized?: () => void,
): Promise<string> {
  const data = await authJson<{ url: string }>(
    `/api/audio/${encodeURIComponent(trackId)}/url`,
    "api.streamUrlFailed",
    {},
    onUnauthorized,
  );
  return data.url;
}

export async function createPlaylist(
  name: string,
  onUnauthorized?: () => void,
): Promise<Playlist> {
  const data = await authJson<{ playlist: Playlist }>(
    "/api/playlists",
    "api.createPlaylistFailed",
    { method: "POST", body: JSON.stringify({ name }) },
    onUnauthorized,
  );
  return data.playlist;
}

export async function renamePlaylist(
  playlistId: string,
  name: string,
  onUnauthorized?: () => void,
): Promise<void> {
  await authOk(
    `/api/playlists/${encodeURIComponent(playlistId)}`,
    "api.renamePlaylistFailed",
    { method: "PATCH", body: JSON.stringify({ name }) },
    onUnauthorized,
  );
}

export async function deletePlaylist(
  playlistId: string,
  onUnauthorized?: () => void,
): Promise<void> {
  await authOk(
    `/api/playlists/${encodeURIComponent(playlistId)}`,
    "api.deletePlaylistFailed",
    { method: "DELETE" },
    onUnauthorized,
  );
}

export async function addToPlaylist(
  playlistId: string,
  trackId: string,
  onUnauthorized?: () => void,
): Promise<{ message?: string }> {
  return authJson(
    `/api/playlists/${encodeURIComponent(playlistId)}/tracks`,
    "api.addToPlaylistFailed",
    { method: "POST", body: JSON.stringify({ track_id: trackId }) },
    onUnauthorized,
  );
}

export async function removeFromPlaylist(
  playlistId: string,
  trackId: string,
  onUnauthorized?: () => void,
): Promise<void> {
  await authOk(
    `/api/playlists/${encodeURIComponent(playlistId)}/tracks/${encodeURIComponent(trackId)}`,
    "api.removeFromPlaylistFailed",
    { method: "DELETE" },
    onUnauthorized,
  );
}

export async function bulkDeleteTracks(
  trackIds: string[],
  onUnauthorized?: () => void,
): Promise<{ deleted: number }> {
  return authJson(
    "/api/tracks/bulk-delete",
    "api.bulkDeleteFailed",
    { method: "POST", body: JSON.stringify({ track_ids: trackIds }) },
    onUnauthorized,
  );
}

export async function bulkAddToPlaylist(
  playlistId: string,
  trackIds: string[],
  onUnauthorized?: () => void,
): Promise<{ message?: string }> {
  return authJson(
    `/api/playlists/${encodeURIComponent(playlistId)}/tracks/bulk`,
    "api.bulkAddToPlaylistFailed",
    { method: "POST", body: JSON.stringify({ track_ids: trackIds }) },
    onUnauthorized,
  );
}

export async function searchYouTube(
  query: string,
  onUnauthorized?: () => void,
): Promise<Track[]> {
  const data = await authJson<{ results?: Track[] }>(
    `/api/search?q=${encodeURIComponent(query)}`,
    "api.searchFailed",
    {},
    onUnauthorized,
  );
  return data.results ?? [];
}

export async function playTracks(
  trackId: string,
  deviceIds: string[],
  onUnauthorized?: () => void,
): Promise<{ message?: string }> {
  return authJson(
    "/api/play",
    "api.playFailed",
    {
      method: "POST",
      body: JSON.stringify({ track_id: trackId, device_ids: deviceIds }),
    },
    onUnauthorized,
  );
}

export async function queueNext(
  trackId: string,
  deviceIds: string[],
  onUnauthorized?: () => void,
): Promise<{ message?: string }> {
  return authJson(
    "/api/queue",
    "api.queueFailed",
    {
      method: "POST",
      body: JSON.stringify({ track_id: trackId, device_ids: deviceIds }),
    },
    onUnauthorized,
  );
}

export async function removeQueueItem(
  deviceId: string,
  entry: string,
  onUnauthorized?: () => void,
): Promise<void> {
  await authOk(
    `/api/devices/${encodeURIComponent(deviceId)}/queue/${encodeURIComponent(entry)}`,
    "api.removeQueueFailed",
    { method: "DELETE" },
    onUnauthorized,
  );
}

export async function clearQueue(
  deviceId: string,
  onUnauthorized?: () => void,
): Promise<void> {
  await authOk(
    `/api/devices/${encodeURIComponent(deviceId)}/queue`,
    "api.clearQueueFailed",
    { method: "DELETE" },
    onUnauthorized,
  );
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
