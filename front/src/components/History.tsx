import { useEffect, useRef, useState } from "react";
import { authFetch, fetchTracks, PER_PAGE } from "../api";
import { ScrollingText } from "./ScrollingText";
import type { Track, TracksPage } from "../types";

function formatDuration(seconds?: number): string {
  if (!seconds) return "--:--";
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${m}:${s.toString().padStart(2, "0")}`;
}

interface Props {
  active: boolean;
  // 認証確認時に取得済みの 1 ページ目。初回フェッチの代わりに使う
  initialData: TracksPage | null;
  refreshKey: number;
  currentTrack: Track | null;
  onSelectTrack: (track: Track) => void;
  onTrackDeleted: (trackId: string) => void;
  onUnauthorized: () => void;
  showToast: (msg: string) => void;
}

export function History({ active, initialData, refreshKey, currentTrack, onSelectTrack, onTrackDeleted, onUnauthorized, showToast }: Props) {
  const [page, setPage] = useState(1);
  const [tracks, setTracks] = useState<Track[]>([]);
  const [total, setTotal] = useState(0);
  // WS 切断中でも REST 操作後にリストを更新できるようにするローカルカウンター
  const [localVersion, setLocalVersion] = useState(0);
  // 消費済みの initialData を識別し、再認証などで新しいスナップショットが
  // 渡されたときはあらためて消費できるようにする
  const consumedInitial = useRef<TracksPage | null>(null);

  const totalPages = Math.max(1, Math.ceil(total / PER_PAGE));

  useEffect(() => {
    if (!active) return;
    if (initialData && consumedInitial.current !== initialData) {
      consumedInitial.current = initialData;
      // 表示中のページと一致する場合のみ採用。ずれていれば通常のフェッチへ
      if (page === initialData.page) {
        setTracks(initialData.tracks);
        setTotal(initialData.total);
        return;
      }
    }
    let cancelled = false;
    fetchTracks(page, PER_PAGE, onUnauthorized)
      .then((data) => {
        if (cancelled) return;
        setTracks(data.tracks);
        setTotal(data.total);
        // 削除でページが範囲外になったら最終ページへ戻す
        const last = Math.max(1, Math.ceil(data.total / PER_PAGE));
        if (page > last) setPage(last);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [active, initialData, page, refreshKey, localVersion, onUnauthorized]);

  if (total === 0) return null;

  async function deleteTrack(trackId: string) {
    try {
      const res = await authFetch(
        `/api/tracks/${encodeURIComponent(trackId)}`,
        { method: "DELETE" },
        onUnauthorized,
      );
      if (!res.ok) throw new Error("削除に失敗しました");
      onTrackDeleted(trackId);
      setLocalVersion((v) => v + 1);
      showToast("トラックを削除しました");
    } catch (e) {
      showToast(`エラー: ${(e as Error).message}`);
    }
  }

  return (
    <div className="history-section">
      <div className="section-label">取得済みトラック ({total})</div>
      <div className="history-list">
        {tracks.map((t) => {
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
                <ScrollingText className="history-title" text={t.title} />
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

      {totalPages > 1 && (
        <div className="pagination">
          <button
            className="btn btn-outline btn-sm"
            disabled={page <= 1}
            onClick={() => setPage(page - 1)}
          >
            前へ
          </button>
          <span className="pagination-info">
            {page} / {totalPages}
          </span>
          <button
            className="btn btn-outline btn-sm"
            disabled={page >= totalPages}
            onClick={() => setPage(page + 1)}
          >
            次へ
          </button>
        </div>
      )}
    </div>
  );
}
