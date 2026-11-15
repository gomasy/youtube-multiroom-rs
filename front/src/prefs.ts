const STREAM_MODE_KEY = "stream_mode";

/** The streaming toggle as last left in this browser (storage may be unavailable) */
export function loadStreamMode(): boolean {
  try {
    return localStorage.getItem(STREAM_MODE_KEY) === "1";
  } catch {
    return false;
  }
}

export function saveStreamMode(stream: boolean) {
  try {
    localStorage.setItem(STREAM_MODE_KEY, stream ? "1" : "0");
  } catch { /* only the remembered default is lost */ }
}
