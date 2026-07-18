import { useEffect, useRef, useState } from "react";
import {
  addToPlaylist,
  authFetch,
  createPlaylist,
  deletePlaylist,
  fetchTracks,
  removeFromPlaylist,
  reorderTrack,
  PER_PAGE,
} from "../api";
import { TrackRowInfo } from "./TrackRowInfo";
import { AddToPlaylistMenu } from "./AddToPlaylistMenu";
import { AddToListIcon, CloseIcon, TrashIcon } from "./icons";
import type { Playlist, Track, TracksPage } from "../types";

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
  playlists: Playlist[];
  /** REST での作成に成功したとき、親の一覧へ楽観的に反映させる */
  onPlaylistCreated: (playlist: Playlist) => void;
  onSelectTrack: (track: Track) => void;
  onTrackDeleted: (trackId: string) => void;
  onUnauthorized: () => void;
  showToast: (msg: string) => void;
}

export function History({ active, initialData, refreshKey, currentTrack, playlists, onPlaylistCreated, onSelectTrack, onTrackDeleted, onUnauthorized, showToast }: Props) {
  const [page, setPage] = useState(1);
  const [tracks, setTracks] = useState<Track[]>([]);
  const [total, setTotal] = useState(0);
  // 表示中のプレイリスト ID (null はライブラリ全体)
  const [viewPlaylist, setViewPlaylist] = useState<string | null>(null);
  // プレイリスト新規作成の入力欄 (null は非表示)
  const [newName, setNewName] = useState<string | null>(null);
  // 「プレイリストに追加」メニューを開いているトラック ID
  const [menuTrackId, setMenuTrackId] = useState<string | null>(null);
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
  const viewingPlaylist = playlists.find((p) => p.id === viewPlaylist) ?? null;

  // 表示中のプレイリストが削除されたらライブラリ表示へ戻す
  useEffect(() => {
    if (viewPlaylist && !playlists.some((p) => p.id === viewPlaylist)) {
      switchView(null);
    }
  }, [playlists, viewPlaylist]);

  useEffect(() => {
    if (!active) return;
    if (!viewPlaylist && initialData && consumedInitial.current !== initialData) {
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
    fetchTracks(page, PER_PAGE, onUnauthorized, undefined, viewPlaylist)
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
  }, [active, initialData, page, refreshKey, localVersion, viewPlaylist, onUnauthorized]);

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

  // ライブラリが空でプレイリストも無いうちはセクションごと隠す
  if (total === 0 && !viewPlaylist && playlists.length === 0) return null;

  function switchView(playlistId: string | null) {
    setViewPlaylist(playlistId);
    setPage(1);
    setMenuTrackId(null);
    resetDrag();
  }

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
      await reorderTrack(id, newIndex, onUnauthorized, viewPlaylist);
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

  // プレイリスト表示での行の削除ボタン: トラック自体は残して収録から外す
  async function removeTrackFromView(trackId: string) {
    if (!viewPlaylist) return;
    try {
      await removeFromPlaylist(viewPlaylist, trackId, onUnauthorized);
      setLocalVersion((v) => v + 1);
      showToast("プレイリストから外しました");
    } catch (e) {
      showToast(`エラー: ${(e as Error).message}`);
    }
  }

  async function submitNewPlaylist() {
    const name = (newName ?? "").trim();
    if (!name) {
      setNewName(null);
      return;
    }
    try {
      const playlist = await createPlaylist(name, onUnauthorized);
      setNewName(null);
      showToast(`プレイリスト「${playlist.name}」を作成しました`);
      // 親の一覧へ先に反映してから切り替える。playlists_update を待つと、
      // その間「存在しないプレイリスト」としてライブラリへ弾き返されてしまう
      onPlaylistCreated(playlist);
      switchView(playlist.id);
    } catch (e) {
      showToast(`エラー: ${(e as Error).message}`);
    }
  }

  async function deleteViewingPlaylist() {
    if (!viewingPlaylist) return;
    try {
      await deletePlaylist(viewingPlaylist.id, onUnauthorized);
      showToast(`プレイリスト「${viewingPlaylist.name}」を削除しました`);
      switchView(null);
    } catch (e) {
      showToast(`エラー: ${(e as Error).message}`);
    }
  }

  async function addTrackToPlaylist(playlistId: string, trackId: string) {
    setMenuTrackId(null);
    try {
      const data = await addToPlaylist(playlistId, trackId, onUnauthorized);
      showToast(data.message || "プレイリストに追加しました");
    } catch (e) {
      showToast(`エラー: ${(e as Error).message}`);
    }
  }

  return (
    <div className="history-section">
      <div className="playlist-bar">
        <button
          className={`playlist-tab${viewPlaylist === null ? " active" : ""}`}
          onClick={() => switchView(null)}
        >
          ライブラリ
        </button>
        {playlists.map((p) => (
          <button
            key={p.id}
            className={`playlist-tab${viewPlaylist === p.id ? " active" : ""}`}
            onClick={() => switchView(p.id)}
          >
            {p.name} <span className="playlist-tab-count">{p.count}</span>
          </button>
        ))}
        {newName === null ? (
          <button
            className="playlist-tab playlist-tab-add"
            title="プレイリストを作成"
            onClick={() => setNewName("")}
          >
            ＋
          </button>
        ) : (
          <span className="playlist-new">
            <input
              type="text"
              className="playlist-new-input"
              placeholder="プレイリスト名"
              autoFocus
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") submitNewPlaylist();
                if (e.key === "Escape") setNewName(null);
              }}
            />
            <button className="btn btn-sm" onClick={submitNewPlaylist}>
              作成
            </button>
            <button className="text-btn" onClick={() => setNewName(null)}>
              キャンセル
            </button>
          </span>
        )}
      </div>

      <div className="section-label history-header">
        <span>
          {viewingPlaylist
            ? `${viewingPlaylist.name} (${total})`
            : `取得済みトラック (${total})`}
        </span>
        {viewingPlaylist && (
          <button
            className="text-btn text-btn-danger"
            onClick={deleteViewingPlaylist}
          >
            プレイリストを削除
          </button>
        )}
      </div>

      {total === 0 && (
        <div className="history-empty">
          {viewPlaylist
            ? "このプレイリストは空です。ライブラリの ♪＋ ボタンで追加できます"
            : "トラックがありません"}
        </div>
      )}

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
              {!viewPlaylist && (
                <span className="playlist-menu-anchor" onClick={(e) => e.stopPropagation()}>
                  <button
                    className="delete-btn add-btn"
                    title="プレイリストに追加"
                    onClick={() => setMenuTrackId(menuTrackId === t.id ? null : t.id)}
                  >
                    <AddToListIcon />
                  </button>
                  {menuTrackId === t.id && (
                    <AddToPlaylistMenu
                      playlists={playlists}
                      onAdd={(pid) => addTrackToPlaylist(pid, t.id)}
                      onClose={() => setMenuTrackId(null)}
                    />
                  )}
                </span>
              )}
              {/* プレイリスト表示では収録から外すだけで、トラック自体は消さない */}
              <button
                className="delete-btn"
                title={viewPlaylist ? "プレイリストから外す" : "トラックを削除"}
                onClick={(e) => {
                  e.stopPropagation();
                  if (viewPlaylist) removeTrackFromView(t.id);
                  else deleteTrack(t.id);
                }}
              >
                {viewPlaylist ? <CloseIcon /> : <TrashIcon />}
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
