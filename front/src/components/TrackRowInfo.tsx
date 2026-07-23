import { ScrollingText } from "./ScrollingText";
import { formatDuration } from "../format";
import type { Track } from "../types";

export function TrackRowInfo({ track }: { track: Track }) {
  return (
    <>
      {track.thumbnail && (
        <img
          className="history-thumb"
          src={track.thumbnail}
          alt=""
          draggable={false}
          onError={(e) => { (e.target as HTMLImageElement).style.display = "none"; }}
        />
      )}
      <div className="history-info">
        <ScrollingText className="history-title" text={track.title} />
        <div className="history-meta">
          {track.channel ? `${track.channel} · ` : ""}
          {track.is_live
            ? <span className="live-badge">LIVE</span>
            : formatDuration(track.duration)}
        </div>
      </div>
    </>
  );
}
