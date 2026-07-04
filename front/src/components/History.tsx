import { useEffect, useRef, useState } from "react";
import { authFetch, fetchTracks, reorderTrack, PER_PAGE } from "../api";
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
  // ドラッグ&ドロップ並べ替えの状態。ドラッグ中に WS 通知などで一覧が
  // 入れ替わってもよいように、対象はインデックスではなく ID で追跡する
  const [dragId, setDragId] = useState<string | null>(null);
  // 挿入位置 (0 〜 tracks.length)。i は「i 番目の前」を意味する
  const [dropIndex, setDropIndex] = useState<number | null>(null);
  const listRef = useRef<HTMLDivElement>(null);

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

  // ポインタ位置から挿入インデックスを求める (各行の中央を境に前後を判定)
  function updateDropIndex(clientY: number) {
    const list = listRef.current;
    if (!list) return;
    const items = list.querySelectorAll<HTMLElement>(".history-item");
    let idx = items.length;
    for (let i = 0; i < items.length; i++) {
      const rect = items[i].getBoundingClientRect();
      if (clientY < rect.top + rect.height / 2) {
        idx = i;
        break;
      }
    }
    setDropIndex(idx);
  }

  function resetDrag() {
    setDragId(null);
    setDropIndex(null);
  }

  // マウス・タッチ共通の Pointer Events でドラッグする。
  // ハンドルが setPointerCapture するので move/up はハンドル上で受けられる
  function handleDragStart(e: React.PointerEvent<HTMLElement>, trackId: string) {
    if (tracks.length < 2) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    setDragId(trackId);
    updateDropIndex(e.clientY);
  }

  function handleDragMove(e: React.PointerEvent<HTMLElement>) {
    if (dragId === null) return;
    // 画面端に近づいたらページをスクロールして続きを見せる
    if (e.clientY < 70) {
      window.scrollBy({ top: -14 });
    } else if (e.clientY > window.innerHeight - 70) {
      window.scrollBy({ top: 14 });
    }
    updateDropIndex(e.clientY);
  }

  // ドロップ確定: ローカルを楽観的に並べ替えてからサーバーへ保存する。
  // 確定表示はサーバーが tracks_update で通知してくる再取得に任せる
  async function commitReorder() {
    if (dragId === null || dropIndex === null) return;
    // ドラッグ中に一覧が更新された場合に備え、現在の配列から位置を引き直す
    const from = tracks.findIndex((t) => t.id === dragId);
    let to = dropIndex;
    resetDrag();
    if (from === -1) return; // ドラッグ中に削除された
    if (to === from || to === from + 1) return; // 位置が変わらない
    if (to > from) to -= 1; // 自分自身を除いた後の挿入位置に補正

    const next = [...tracks];
    const [moved] = next.splice(from, 1);
    next.splice(to, 0, moved);
    setTracks(next);

    try {
      await reorderTrack(moved.id, (page - 1) * PER_PAGE + to, onUnauthorized);
    } catch (e) {
      setLocalVersion((v) => v + 1); // サーバー側の並びに戻す
      showToast(`エラー: ${(e as Error).message}`);
    }
  }

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
      <div className="history-list" ref={listRef}>
        {tracks.map((t, i) => {
          const isCurrent = currentTrack?.id === t.id;
          const classes = ["history-item"];
          if (dragId === t.id) classes.push("dragging");
          if (dropIndex === i) classes.push("drop-before");
          if (i === tracks.length - 1 && dropIndex === tracks.length) {
            classes.push("drop-after");
          }
          return (
            <div
              key={t.id}
              className={classes.join(" ")}
              style={isCurrent ? { borderColor: "var(--accent)" } : undefined}
              onClick={() => onSelectTrack(t)}
            >
              {tracks.length > 1 && (
                <span
                  className="drag-handle"
                  title="ドラッグで並べ替え"
                  onClick={(e) => e.stopPropagation()}
                  onPointerDown={(e) => handleDragStart(e, t.id)}
                  onPointerMove={handleDragMove}
                  onPointerUp={() => commitReorder()}
                  onPointerCancel={resetDrag}
                >
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor">
                    <circle cx="9" cy="5" r="1.7" />
                    <circle cx="15" cy="5" r="1.7" />
                    <circle cx="9" cy="12" r="1.7" />
                    <circle cx="15" cy="12" r="1.7" />
                    <circle cx="9" cy="19" r="1.7" />
                    <circle cx="15" cy="19" r="1.7" />
                  </svg>
                </span>
              )}
              {t.thumbnail && (
                <img
                  className="history-thumb"
                  src={t.thumbnail}
                  alt=""
                  draggable={false}
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
