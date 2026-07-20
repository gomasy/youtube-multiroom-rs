/** Format seconds as m:ss */
export function formatTime(seconds: number): string {
  const total = Math.floor(seconds);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${s.toString().padStart(2, "0")}`;
}

/** Format track duration for display. Shows "--:--" when undefined or 0. */
export function formatDuration(seconds?: number): string {
  return seconds ? formatTime(seconds) : "--:--";
}
