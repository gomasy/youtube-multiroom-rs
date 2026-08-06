import { t, lang } from "./i18n";
import type { CacheStatus, Playlist, Track, TracksPage } from "./types";

export const PER_PAGE = 10;

let apiToken = localStorage.getItem("api_token");

/**
 * Thrown on a 401. Callers that already react via onUnauthorized (auth modal)
 * can use this to avoid also showing a redundant error toast.
 */
export class UnauthorizedError extends Error {}

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
  // Copied rather than assigned into `options`, which belongs to the caller: a
  // reused request must not inherit an earlier token.
  const res = await fetch(url, {
    ...options,
    headers: { ...authHeaders(), ...(options.headers as Record<string, string>) },
  });
  if (res.status === 401) {
    onUnauthorized?.();
    throw new UnauthorizedError(t("api.unauthorized"));
  }
  return res;
}

/** Perform an authFetch and throw t(errorKey) on a non-OK response. */
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

// Path builders for the resources addressed by ID. Routed through here so no
// call site can forget to escape an ID into the URL.
const playlistPath = (playlistId: string) =>
  `/api/playlists/${encodeURIComponent(playlistId)}`;
const devicePath = (deviceId: string) => `/api/devices/${encodeURIComponent(deviceId)}`;

/** authOk that parses the response body as JSON. */
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
  signal?: AbortSignal,
): Promise<string> {
  const data = await authJson<{ url: string }>(
    `/api/audio/${encodeURIComponent(trackId)}/url`,
    "api.streamUrlFailed",
    { signal },
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
    playlistPath(playlistId),
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
    playlistPath(playlistId),
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
    `${playlistPath(playlistId)}/tracks`,
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
    `${playlistPath(playlistId)}/tracks/${encodeURIComponent(trackId)}`,
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

/**
 * Start a background re-fetch of the given tracks' metadata. `total` is how
 * many tracks the job will visit; the refresh itself lands later, as
 * tracks_update frames.
 */
export async function refreshTracksMetadata(
  trackIds: string[],
  onUnauthorized?: () => void,
): Promise<{ total: number; message?: string }> {
  return authJson(
    "/api/tracks/refresh-metadata",
    "api.refreshMetadataFailed",
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
    `${playlistPath(playlistId)}/tracks/bulk`,
    "api.bulkAddToPlaylistFailed",
    { method: "POST", body: JSON.stringify({ track_ids: trackIds }) },
    onUnauthorized,
  );
}

export async function bulkRemoveFromPlaylist(
  playlistId: string,
  trackIds: string[],
  onUnauthorized?: () => void,
): Promise<{ removed: number }> {
  return authJson(
    `${playlistPath(playlistId)}/tracks/bulk-remove`,
    "api.bulkRemoveFromPlaylistFailed",
    { method: "POST", body: JSON.stringify({ track_ids: trackIds }) },
    onUnauthorized,
  );
}

export async function searchYouTube(
  query: string,
  onUnauthorized?: () => void,
  signal?: AbortSignal,
): Promise<Track[]> {
  const data = await authJson<{ results?: Track[] }>(
    `/api/search?q=${encodeURIComponent(query)}`,
    "api.searchFailed",
    { signal },
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
    `${devicePath(deviceId)}/queue/${encodeURIComponent(entry)}`,
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
    `${devicePath(deviceId)}/queue`,
    "api.clearQueueFailed",
    { method: "DELETE" },
    onUnauthorized,
  );
}

/**
 * Line every other device up with this one, starting from where it is now.
 * The empty body is what asks for "all the others"; the endpoint also accepts
 * an explicit list.
 */
export async function syncDevices(
  deviceId: string,
  onUnauthorized?: () => void,
): Promise<{ message?: string }> {
  return authJson(
    `${devicePath(deviceId)}/sync`,
    "api.syncFailed",
    { method: "POST", body: "{}" },
    onUnauthorized,
  );
}

export async function fetchCacheStatus(
  onUnauthorized?: () => void,
): Promise<CacheStatus> {
  return authJson("/api/cache", "api.cacheStatusFailed", {}, onUnauthorized);
}

export async function cleanupCache(
  onUnauthorized?: () => void,
): Promise<{ message?: string }> {
  return authJson(
    "/api/cache/cleanup",
    "api.cacheCleanupFailed",
    { method: "POST" },
    onUnauthorized,
  );
}

/**
 * Start re-downloading the tracks whose cached audio is gone. The files
 * themselves land later, as tracks_update frames.
 */
export async function repairCache(
  onUnauthorized?: () => void,
): Promise<{ message?: string }> {
  return authJson(
    "/api/cache/repair",
    "api.cacheRepairFailed",
    { method: "POST" },
    onUnauthorized,
  );
}

export async function checkAuth(
  token?: string,
): Promise<{ authorized: boolean; data: TracksPage | null }> {
  try {
    const data = await fetchTracks(1, PER_PAGE, undefined, token);
    return { authorized: true, data };
  } catch (error) {
    if (error instanceof UnauthorizedError) {
      return { authorized: false, data: null };
    }
    throw error;
  }
}
