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
  // Lets the server localize API messages.
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

/**
 * Perform an authFetch and throw t(errorKey) on a non-OK response, or
 * t(conflictKey) on a 409 when one is given.
 */
async function authOk(
  url: string,
  errorKey: string,
  options: RequestInit = {},
  onUnauthorized?: () => void,
  conflictKey?: string,
): Promise<Response> {
  const res = await authFetch(url, options, onUnauthorized);
  if (conflictKey && res.status === 409) throw new Error(t(conflictKey));
  if (!res.ok) throw new Error(t(errorKey));
  return res;
}

/** Request options for a JSON body. */
const json = (body: unknown, method = "POST"): RequestInit => ({
  method,
  body: JSON.stringify(body),
});

// Path builders for the resources addressed by ID. Routed through here so no
// call site can forget to escape an ID into the URL.
const playlistPath = (playlistId: string) =>
  `/api/playlists/${encodeURIComponent(playlistId)}`;
const trackPath = (trackId: string) => `/api/tracks/${encodeURIComponent(trackId)}`;
const devicePath = (deviceId: string) => `/api/devices/${encodeURIComponent(deviceId)}`;

export async function deleteTrack(trackId: string, onUnauthorized?: () => void): Promise<void> {
  await authOk(trackPath(trackId), "history.deleteFailed", { method: "DELETE" }, onUnauthorized);
}

/** For the endpoints that name a playlist, where a 409 means the name is taken. */
function playlistNameOk(
  url: string,
  errorKey: string,
  options: RequestInit,
  onUnauthorized?: () => void,
): Promise<Response> {
  return authOk(url, errorKey, options, onUnauthorized, "api.playlistNameTaken");
}

/** authOk that parses the response body as JSON. */
async function authJson<T>(
  url: string,
  errorKey: string,
  options: RequestInit = {},
  onUnauthorized?: () => void,
): Promise<T> {
  return (await authOk(url, errorKey, options, onUnauthorized)).json();
}

/** POST `{ track_ids }`, the body of every bulk track endpoint. */
function postTrackIds<T>(
  url: string,
  errorKey: string,
  trackIds: string[],
  onUnauthorized?: () => void,
): Promise<T> {
  return authJson(url, errorKey, json({ track_ids: trackIds }), onUnauthorized);
}

/** POST a track and the devices it is for. */
function postTrackToDevices(
  url: string,
  errorKey: string,
  trackId: string,
  deviceIds: string[],
  onUnauthorized?: () => void,
): Promise<{ message?: string }> {
  return authJson(
    url,
    errorKey,
    json({ track_id: trackId, device_ids: deviceIds }),
    onUnauthorized,
  );
}

export async function fetchTracks(
  page: number,
  onUnauthorized?: () => void,
  token?: string,
  playlistId?: string | null,
  filter?: string,
): Promise<TracksPage> {
  let url = `/api/tracks?page=${page}&per_page=${PER_PAGE}`;
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
    json({ track_id: trackId, new_index: newIndex, playlist: playlistId ?? null }),
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

/** A streamed track's signed relay URL, opened `startSec` into the track */
export function relayUrl(streamUrl: string, startSec: number): string {
  return startSec > 0 ? `${streamUrl}&start=${Math.round(startSec * 1000)}` : streamUrl;
}

export async function createPlaylist(
  name: string,
  onUnauthorized?: () => void,
): Promise<Playlist> {
  const res = await playlistNameOk(
    "/api/playlists",
    "api.createPlaylistFailed",
    json({ name }),
    onUnauthorized,
  );
  const data: { playlist: Playlist } = await res.json();
  return data.playlist;
}

export async function reorderPlaylist(
  playlistId: string,
  newIndex: number,
  onUnauthorized?: () => void,
): Promise<void> {
  await authOk(
    "/api/playlists/reorder",
    "api.reorderFailed",
    json({ playlist_id: playlistId, new_index: newIndex }),
    onUnauthorized,
  );
}

export async function renamePlaylist(
  playlistId: string,
  name: string,
  onUnauthorized?: () => void,
): Promise<void> {
  await playlistNameOk(
    playlistPath(playlistId),
    "api.renamePlaylistFailed",
    json({ name }, "PATCH"),
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
    json({ track_id: trackId }),
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
  return postTrackIds("/api/tracks/bulk-delete", "api.bulkDeleteFailed", trackIds, onUnauthorized);
}

/** Start a background re-fetch of the given tracks' metadata; `total` is how many. */
export async function refreshTracksMetadata(
  trackIds: string[],
  onUnauthorized?: () => void,
): Promise<{ total: number; message?: string }> {
  return postTrackIds("/api/tracks/refresh-metadata", "api.refreshMetadataFailed", trackIds, onUnauthorized);
}

/** Start downloading the streamed tracks among those given; `total` is how many. */
export async function downloadTracks(
  trackIds: string[],
  onUnauthorized?: () => void,
): Promise<{ total: number; message?: string }> {
  return postTrackIds("/api/tracks/download", "api.downloadTracksFailed", trackIds, onUnauthorized);
}

/** Turn the cached tracks among those given into streamed ones, deleting their files. */
export async function releaseTracks(
  trackIds: string[],
  onUnauthorized?: () => void,
): Promise<{ released: number; message?: string }> {
  return postTrackIds("/api/tracks/release", "api.releaseTracksFailed", trackIds, onUnauthorized);
}

/**
 * Add the videos a playlist's YouTube source has gained; `total` is how many
 * (zero: up to date). `stream` registers them without downloading. A 409
 * means an import into the playlist is still running.
 */
export async function syncPlaylist(
  playlistId: string,
  stream: boolean,
  onUnauthorized?: () => void,
): Promise<{ total: number; message?: string }> {
  const res = await authOk(
    `${playlistPath(playlistId)}/sync`,
    "api.syncPlaylistFailed",
    json({ stream }),
    onUnauthorized,
    "api.syncPlaylistBusy",
  );
  return res.json();
}

export async function bulkAddToPlaylist(
  playlistId: string,
  trackIds: string[],
  onUnauthorized?: () => void,
): Promise<{ message?: string }> {
  return postTrackIds(`${playlistPath(playlistId)}/tracks/bulk`, "api.bulkAddToPlaylistFailed", trackIds, onUnauthorized);
}

export async function bulkRemoveFromPlaylist(
  playlistId: string,
  trackIds: string[],
  onUnauthorized?: () => void,
): Promise<{ removed: number }> {
  return postTrackIds(`${playlistPath(playlistId)}/tracks/bulk-remove`, "api.bulkRemoveFromPlaylistFailed", trackIds, onUnauthorized);
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
  return postTrackToDevices("/api/play", "api.playFailed", trackId, deviceIds, onUnauthorized);
}

export async function queueNext(
  trackId: string,
  deviceIds: string[],
  onUnauthorized?: () => void,
): Promise<{ message?: string }> {
  return postTrackToDevices("/api/queue", "api.queueFailed", trackId, deviceIds, onUnauthorized);
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

export async function deleteDevice(deviceId: string, onUnauthorized?: () => void): Promise<void> {
  await authOk(devicePath(deviceId), "devices.deleteFailed", { method: "DELETE" }, onUnauthorized);
}

export async function cancelPending(deviceId: string, onUnauthorized?: () => void): Promise<void> {
  await authOk(
    `${devicePath(deviceId)}/pending`,
    "api.cancelPendingFailed",
    { method: "DELETE" },
    onUnauthorized,
  );
}

export async function seekDevice(
  deviceId: string,
  positionMs: number,
  onUnauthorized?: () => void,
): Promise<void> {
  await authOk(
    `${devicePath(deviceId)}/seek`,
    "devices.seekFailed",
    json({ position_ms: positionMs }),
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

/** Line every other device up with this one; the empty body asks for "all the others". */
export async function syncDevices(
  deviceId: string,
  onUnauthorized?: () => void,
): Promise<{ message?: string }> {
  return authJson(
    `${devicePath(deviceId)}/sync`,
    "api.syncFailed",
    json({}),
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

/** Start re-downloading the tracks whose cached audio is gone. */
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

/** The export document as text: it is on its way to a file. */
export async function exportLibrary(onUnauthorized?: () => void): Promise<string> {
  return (
    await authOk("/api/library/export", "api.exportFailed", {}, onUnauthorized)
  ).text();
}

export async function importLibrary(
  doc: unknown,
  onUnauthorized?: () => void,
): Promise<{ message?: string }> {
  return authJson(
    "/api/library/import",
    "api.importFailed",
    json(doc),
    onUnauthorized,
  );
}

export async function checkAuth(
  token?: string,
): Promise<{ authorized: boolean; data: TracksPage | null }> {
  try {
    const data = await fetchTracks(1, undefined, token);
    return { authorized: true, data };
  } catch (error) {
    if (error instanceof UnauthorizedError) {
      return { authorized: false, data: null };
    }
    throw error;
  }
}
