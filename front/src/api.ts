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
    throw new Error("認証が必要です");
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
  if (!res.ok) throw new Error("トラック一覧の取得に失敗しました");
  return res.json();
}

// トラックを並びの newIndex (0 始まり) へ移動する
// (playlistId 指定時はプレイリスト内、未指定はライブラリ全体)
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
  if (!res.ok) throw new Error("並べ替えに失敗しました");
}

// プレビュー再生用に audio 要素へ渡せるストリーム URL (署名付き) を取得する
export async function getStreamUrl(
  trackId: string,
  onUnauthorized?: () => void,
): Promise<string> {
  const res = await authFetch(
    `/api/audio/${encodeURIComponent(trackId)}/url`,
    {},
    onUnauthorized,
  );
  if (!res.ok) throw new Error("再生 URL の取得に失敗しました");
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
  if (!res.ok) throw new Error("プレイリストの作成に失敗しました");
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
  if (!res.ok) throw new Error("プレイリストの削除に失敗しました");
}

// トラックをプレイリスト末尾へ追加する (収録済みなら末尾へ移動)
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
  if (!res.ok) throw new Error("プレイリストへの追加に失敗しました");
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
  if (!res.ok) throw new Error("プレイリストからの削除に失敗しました");
}

// yt-dlp の ytsearch で YouTube を検索し、Track 互換の結果一覧を返す
export async function searchYouTube(
  query: string,
  onUnauthorized?: () => void,
): Promise<Track[]> {
  const res = await authFetch(
    `/api/search?q=${encodeURIComponent(query)}`,
    {},
    onUnauthorized,
  );
  if (!res.ok) throw new Error("検索に失敗しました");
  const data = await res.json();
  return data.results ?? [];
}

// トラックを選択デバイスで即時再生するようキューする
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
  if (!res.ok) throw new Error("再生のキューに失敗しました");
  return res.json();
}

// トラックを選択デバイスの「次に再生」キュー末尾へ追加する
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
  if (!res.ok) throw new Error("キューへの追加に失敗しました");
  return res.json();
}

// キュー項目を一意なエントリ値の指定で 1 件削除する
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
  if (!res.ok) throw new Error("キューからの削除に失敗しました");
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
  if (!res.ok) throw new Error("キューのクリアに失敗しました");
}

// 認証確認を兼ねてトラック一覧の先頭ページを取得する。
// token を渡すと保存済みトークンの代わりにそれで検証する(モーダルでの入力確認用)。
// authorized=false は 401(要認証)。ネットワークエラー等は認証済み扱いで進める。
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
