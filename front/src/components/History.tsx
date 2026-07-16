import { useEffect, useRef, useState } from "react";
import { authFetch, fetchTracks, reorderTrack, PER_PAGE } from "../api";
import { TrackRowInfo } from "./TrackRowInfo";
import type { Track, TracksPage } from "../types";

// 総件数から最終ページ番号 (1 始まり) を求める
function lastPage(total: number): number {
  return Math.max(1, Math.ceil(total / PER_PAGE));
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
  // ページをまたいでドロップしたとき用に、開始時のトラックと全体位置を控える
  const dragOrigin = useRef<{ track: Track; globalIndex: number } | null>(null);
  // ドラッグ中にページ送りボタンへかざしている方向 (-1 / 0 / 1)
  const [flipDir, setFlipDir] = useState(0);
  // tracks がどのページの内容か。ページ送り直後はフェッチ完了まで page と
  // ずれるため、ドロップ位置の計算はこちらを基準にする
  const [loadedPage, setLoadedPage] = useState(1);
  const listRef = useRef<HTMLDivElement>(null);
  const prevBtnRef = useRef<HTMLButtonElement>(null);
  const nextBtnRef = useRef<HTMLButtonElement>(null);

  const totalPages = lastPage(total);

  useEffect(() => {
    if (!active) return;
    if (initialData && consumedInitial.current !== initialData) {
      consumedInitial.current = initialData;
      // 表示中のページと一致する場合のみ採用。ずれていれば通常のフェッチへ
      if (page === initialData.page) {
        setTracks(initialData.tracks);
        setTotal(initialData.total);
        setLoadedPage(initialData.page);
        return;
      }
    }
    let cancelled = false;
    fetchTracks(page, PER_PAGE, onUnauthorized)
      .then((data) => {
        if (cancelled) return;
        setTracks(data.tracks);
        setTotal(data.total);
        setLoadedPage(page);
        // 削除でページが範囲外になったら最終ページへ戻す
        const last = lastPage(data.total);
        if (page > last) setPage(last);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [active, initialData, page, refreshKey, localVersion, onUnauthorized]);

  // ドラッグ中のかざし方向に従って一定間隔でページを送る。端に達したら
  // 止めてハイライトも消す。クリーンアップがタイマーの停止を兼ねる
  useEffect(() => {
    if (flipDir === 0) return;
    if (flipDir === -1 ? page <= 1 : page >= totalPages) {
      setFlipDir(0);
      return;
    }
    const timer = window.setInterval(() => setPage((p) => p + flipDir), 650);
    return () => clearInterval(timer);
  }, [flipDir, page, totalPages]);

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
    setFlipDir(0);
    dragOrigin.current = null;
    setDragId(null);
    setDropIndex(null);
  }

  function isOver(el: HTMLElement | null, e: React.PointerEvent) {
    if (!el) return false;
    const r = el.getBoundingClientRect();
    return e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom;
  }

  // マウス・タッチ共通の Pointer Events でドラッグする。ページ送りで行要素が
  // 消えてもドラッグを継続できるよう、残り続けるリスト要素にキャプチャして
  // move/up はリスト側で受ける
  function handleDragStart(e: React.PointerEvent<HTMLElement>, track: Track, index: number) {
    if (total < 2) return;
    e.preventDefault();
    listRef.current?.setPointerCapture(e.pointerId);
    dragOrigin.current = { track, globalIndex: (loadedPage - 1) * PER_PAGE + index };
    setDragId(track.id);
    updateDropIndex(e.clientY);
  }

  function handleDragMove(e: React.PointerEvent<HTMLElement>) {
    if (dragId === null) return;
    // ページ送りボタンにかざしている間は挿入位置ではなくページを切り替える。
    // このとき自動スクロールするとボタンがずれて誤動作するため止めておく
    const dir =
      page > 1 && isOver(prevBtnRef.current, e) ? -1
      : page < totalPages && isOver(nextBtnRef.current, e) ? 1
      : 0;
    setFlipDir(dir);
    if (dir !== 0) {
      setDropIndex(null);
      return;
    }
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
    const id = dragId;
    const to = dropIndex;
    const origin = dragOrigin.current;
    resetDrag();
    if (id === null || to === null || origin === null) return;
    // ドラッグ中に一覧が更新された場合に備え、現在の配列から位置を引き直す。
    // ページをまたいだ場合は現在ページに存在しないので開始時の位置を使う
    const from = tracks.findIndex((t) => t.id === id);
    const origGlobal = from !== -1 ? (loadedPage - 1) * PER_PAGE + from : origin.globalIndex;
    const targetGlobal = (loadedPage - 1) * PER_PAGE + to;
    if (targetGlobal === origGlobal || targetGlobal === origGlobal + 1) return; // 位置が変わらない
    // 自分自身を除いた後の挿入位置に補正
    const newIndex = targetGlobal > origGlobal ? targetGlobal - 1 : targetGlobal;

    const moved = from !== -1 ? tracks[from] : origin.track;
    const next = tracks.filter((t) => t.id !== id);
    next.splice(from !== -1 && from < to ? to - 1 : to, 0, moved);
    setTracks(next.slice(0, PER_PAGE));

    try {
      await reorderTrack(id, newIndex, onUnauthorized);
    } catch (e) {
      showToast(`エラー: ${(e as Error).message}`);
    } finally {
      // 成功時も WS 切断中に備えて REST で取り直し、失敗時はサーバー側の
      // 並びへ戻す
      setLocalVersion((v) => v + 1);
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
      <div
        className="history-list"
        ref={listRef}
        onPointerMove={handleDragMove}
        onPointerUp={() => commitReorder()}
        onPointerCancel={resetDrag}
      >
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
              {total > 1 && (
                <span
                  className="drag-handle"
                  title="ドラッグで並べ替え"
                  onClick={(e) => e.stopPropagation()}
                  onPointerDown={(e) => handleDragStart(e, t, i)}
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
              <TrackRowInfo track={t} />
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
            ref={prevBtnRef}
            className={"btn btn-outline btn-sm" + (flipDir === -1 ? " drag-over" : "")}
            disabled={page <= 1}
            onClick={() => setPage(page - 1)}
          >
            前へ
          </button>
          <span className="pagination-info">
            {page} / {totalPages}
          </span>
          <button
            ref={nextBtnRef}
            className={"btn btn-outline btn-sm" + (flipDir === 1 ? " drag-over" : "")}
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
