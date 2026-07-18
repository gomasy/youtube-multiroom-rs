import { ScrollingText } from "./ScrollingText";
import { PreviewPlayer } from "./PreviewPlayer";
import { formatDuration } from "../format";
import type { Track } from "../types";

interface Props {
  track: Track | null;
  onUnauthorized: () => void;
  showToast: (msg: string) => void;
}

export function NowPlaying({ track, onUnauthorized, showToast }: Props) {
  return (
    <div className={`now-playing${track ? "" : " empty"}`}>
      <div className="track-row">
        {track?.thumbnail ? (
          <img className="track-thumb" src={track.thumbnail} alt="" />
        ) : (
          <div className="track-thumb-placeholder">
            <svg width="24" height="24" viewBox="0 0 24 24" fill="none">
              <path d="M9 18V5l12-2v13" stroke="var(--text-dim)" strokeWidth="1.5" fill="none" />
              <circle cx="6" cy="18" r="3" stroke="var(--text-dim)" strokeWidth="1.5" fill="none" />
              <circle cx="18" cy="16" r="3" stroke="var(--text-dim)" strokeWidth="1.5" fill="none" />
            </svg>
          </div>
        )}
        <div className="track-info">
          <ScrollingText
            className="track-title"
            text={track ? track.title : "曲が選択されていません"}
          />
          <div className="track-meta">
            {track
              ? track.is_live
                ? <>{track.channel ? `${track.channel} · ` : ""}<span className="live-badge">LIVE</span></>
                : [track.channel, formatDuration(track.duration)].filter(Boolean).join(" · ")
              : "YouTube URL を入力して取得してください"}
          </div>
        </div>
      </div>
      {track && (
        // key で再マウントさせ、トラック切り替え時の再生停止と状態リセットを
        // アンマウントに任せる
        <PreviewPlayer
          key={track.id}
          track={track}
          onUnauthorized={onUnauthorized}
          showToast={showToast}
        />
      )}
    </div>
  );
}
