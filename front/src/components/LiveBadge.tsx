import { useEffect, useReducer } from "react";
import { lang, t, tFmt } from "../i18n";
import type { Track } from "../types";

/** LIVE, or the scheduled start while it is still ahead. */
export function LiveBadge({ track }: { track: Track }) {
  const startMs = (track.scheduled_start ?? 0) * 1000;
  const [tick, rerender] = useReducer((n: number) => n + 1, 0);
  // Re-render when the start passes (capped at setTimeout's max delay)
  useEffect(() => {
    const wait = startMs - Date.now();
    if (wait <= 0) return;
    const id = setTimeout(rerender, Math.min(wait, 2 ** 31 - 1));
    return () => clearTimeout(id);
  }, [startMs, tick]);

  if (startMs <= Date.now()) return <span className="live-badge">LIVE</span>;
  const when = new Date(startMs).toLocaleString(lang, {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
  return (
    <span className="upcoming-badge" title={t("track.upcomingHint")}>
      {tFmt("track.upcoming", { when })}
    </span>
  );
}
