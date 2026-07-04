import type { TracksPage } from "./types";

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
): Promise<TracksPage> {
  const res = await authFetch(
    `/api/tracks?page=${page}&per_page=${perPage}`,
    token ? { headers: { Authorization: `Bearer ${token}` } } : {},
    onUnauthorized,
  );
  if (!res.ok) throw new Error("トラック一覧の取得に失敗しました");
  return res.json();
}

// トラックを全体並びの newIndex (0 始まり) へ移動する
export async function reorderTrack(
  trackId: string,
  newIndex: number,
  onUnauthorized?: () => void,
): Promise<void> {
  const res = await authFetch(
    "/api/tracks/reorder",
    {
      method: "POST",
      body: JSON.stringify({ track_id: trackId, new_index: newIndex }),
    },
    onUnauthorized,
  );
  if (!res.ok) throw new Error("並べ替えに失敗しました");
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
