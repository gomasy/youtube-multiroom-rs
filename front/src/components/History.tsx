import { authFetch } from "../api";
import type { Track } from "../types";

function formatDuration(seconds?: number): string {
  if (!seconds) return "--:--";
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${m}:${s.toString().padStart(2, "0")}`;
}

interface Props {
  tracks: Record<string, Track>;
  currentTrack: Track | null;
  onSelectTrack: (track: Track) => void;
  onTrackDeleted: (trackId: string) => void;
  onUnauthorized: () => void;
  showToast: (msg: string) => void;
}

export function History({ tracks, currentTrack, onSelectTrack, onTrackDeleted, onUnauthorized, showToast }: Props) {
  const entries = Object.values(tracks);
  if (entries.length === 0) return null;

  async function deleteTrack(trackId: string) {
    try {
      const res = await authFetch(
        `/api/tracks/${encodeURIComponent(trackId)}`,
        { method: "DELETE" },
        onUnauthorized,
      );
      if (!res.ok) throw new Error("削除に失敗しました");
      onTrackDeleted(trackId);
      showToast("トラックを削除しました");
    } catch (e) {
      showToast(`エラー: ${(e as Error).message}`);
    }
  }

  return (
    <div className="history-section">
      <div className="section-label">取得済みトラック</div>
      <div className="history-list">
        {entries.map((t) => {
          const isCurrent = currentTrack?.id === t.id;
          return (
            <div
              key={t.id}
              className="history-item"
              style={isCurrent ? { borderColor: "var(--accent)" } : undefined}
              onClick={() => onSelectTrack(t)}
            >
              {t.thumbnail && (
                <img
                  className="history-thumb"
                  src={t.thumbnail}
                  alt=""
                  onError={(e) => { (e.target as HTMLImageElement).style.display = "none"; }}
                />
              )}
              <div className="history-info">
                <div className="history-title">{t.title}</div>
                <div className="history-meta">
                  {t.channel ? `${t.channel} · ` : ""}
                  {formatDuration(t.duration)}
                </div>
              </div>
              <button
                className="delete-btn"
                title="トラックを削除"
                onClick={(e) => { e.stopPropagation(); deleteTrack(t.id); }}
              >
                <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                  <path d="M3 6h18M8 6V4a2 2 0 012-2h4a2 2 0 012 2v2m3 0v14a2 2 0 01-2 2H7a2 2 0 01-2-2V6h14" />
                </svg>
              </button>
            </div>
          );
        })}
      </div>
    </div>
  );
}
