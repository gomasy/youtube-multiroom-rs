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

export async function checkAuth(): Promise<boolean> {
  try {
    const res = await fetch("/api/tracks", { headers: authHeaders() });
    return res.status !== 401;
  } catch {
    return true;
  }
}

export async function verifyToken(token: string): Promise<boolean> {
  const h: Record<string, string> = {
    "Content-Type": "application/json",
    Authorization: `Bearer ${token}`,
  };
  try {
    const res = await fetch("/api/tracks", { headers: h });
    return res.status !== 401;
  } catch {
    return true;
  }
}
