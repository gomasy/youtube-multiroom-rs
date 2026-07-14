/** 秒数を m:ss 形式に整形する */
export function formatTime(seconds: number): string {
  const total = Math.floor(seconds);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${s.toString().padStart(2, "0")}`;
}

/** トラックの長さ表示用。未取得 (undefined / 0) は "--:--" */
export function formatDuration(seconds?: number): string {
  return seconds ? formatTime(seconds) : "--:--";
}
