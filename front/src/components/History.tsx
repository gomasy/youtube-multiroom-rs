import { useEffect, useRef, useState } from "react";
import {
  addToPlaylist,
  authOk,
  bulkAddToPlaylist,
  bulkDeleteTracks,
  createPlaylist,
  deletePlaylist,
  fetchTracks,
  removeFromPlaylist,
  renamePlaylist,
  reorderTrack,
  PER_PAGE,
} from "../api";
import { t } from "../i18n";
import { TrackRowInfo } from "./TrackRowInfo";
import { AddToPlaylistMenu } from "./AddToPlaylistMenu";
import { AddToListIcon, CloseIcon, TrashIcon } from "./icons";
import type { Playlist, Track, TracksPage } from "../types";

function lastPage(total: number): number {
  return Math.max(1, Math.ceil(total / PER_PAGE));
}

interface Props {
  active: boolean;
  initialData: TracksPage | null;
  refreshKey: number;
  currentTrack: Track | null;
  playlists: Playlist[];
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
  const [viewPlaylist, setViewPlaylist] = useState<string | null>(null);
  const [newName, setNewName] = useState<string | null>(null);
  const [menuTrackId, setMenuTrackId] = useState<string | null>(null);
  const [localVersion, setLocalVersion] = useState(0);
  const consumedInitial = useRef<TracksPage | null>(null);
  const [dragId, setDragId] = useState<string | null>(null);
  const [dropIndex, setDropIndex] = useState<number | null>(null);
  const dragOrigin = useRef<{ track: Track; globalIndex: number } | null>(null);
  const [flipDir, setFlipDir] = useState(0);
  const [loadedPage, setLoadedPage] = useState(1);
  const listRef = useRef<HTMLDivElement>(null);
  const prevBtnRef = useRef<HTMLButtonElement>(null);
  const nextBtnRef = useRef<HTMLButtonElement>(null);
  const [filterInput, setFilterInput] = useState("");
  const [filter, setFilter] = useState("");
  const filterTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const [selectMode, setSelectMode] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [bulkMenuOpen, setBulkMenuOpen] = useState(false);
  const [renameName, setRenameName] = useState<string | null>(null);

  const totalPages = lastPage(total);
  const viewingPlaylist = playlists.find((p) => p.id === viewPlaylist) ?? null;

  useEffect(() => {
    if (viewPlaylist && !playlists.some((p) => p.id === viewPlaylist)) {
      switchView(null);
    }
  }, [playlists, viewPlaylist]);

  useEffect(() => {
    if (!active) return;
    if (!viewPlaylist && !filter && initialData && consumedInitial.current !== initialData) {
      consumedInitial.current = initialData;
      if (page === initialData.page) {
        setTracks(initialData.tracks);
        setTotal(initialData.total);
        setLoadedPage(initialData.page);
        return;
      }
    }
    let cancelled = false;
    fetchTracks(page, PER_PAGE, onUnauthorized, undefined, viewPlaylist, filter || undefined)
      .then((data) => {
        if (cancelled) return;
        setTracks(data.tracks);
        setTotal(data.total);
        setLoadedPage(page);
        const last = lastPage(data.total);
        if (page > last) setPage(last);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [active, initialData, page, refreshKey, localVersion, viewPlaylist, filter, onUnauthorized]);

  useEffect(() => {
    if (flipDir === 0) return;
    if (flipDir === -1 ? page <= 1 : page >= totalPages) {
      setFlipDir(0);
      return;
    }
    const timer = window.setInterval(() => setPage((p) => p + flipDir), 650);
    return () => clearInterval(timer);
  }, [flipDir, page, totalPages]);

  if (total === 0 && !viewPlaylist && playlists.length === 0) return null;

  function exitSelectMode() {
    setSelectMode(false);
    setSelected(new Set());
    setBulkMenuOpen(false);
  }

  function switchView(playlistId: string | null) {
    setViewPlaylist(playlistId);
    setPage(1);
    setMenuTrackId(null);
    setFilterInput("");
    setFilter("");
    exitSelectMode();
    resetDrag();
  }

  function handleFilterChange(value: string) {
    setFilterInput(value);
    clearTimeout(filterTimer.current);
    filterTimer.current = setTimeout(() => {
      setFilter(value.trim());
      setPage(1);
    }, 300);
  }

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

  function handleDragStart(e: React.PointerEvent<HTMLElement>, track: Track, index: number) {
    if (total < 2 || filter) return;
    e.preventDefault();
    listRef.current?.setPointerCapture(e.pointerId);
    dragOrigin.current = { track, globalIndex: (loadedPage - 1) * PER_PAGE + index };
    setDragId(track.id);
    updateDropIndex(e.clientY);
  }

  function handleDragMove(e: React.PointerEvent<HTMLElement>) {
    if (dragId === null) return;
    const dir =
      page > 1 && isOver(prevBtnRef.current, e) ? -1
      : page < totalPages && isOver(nextBtnRef.current, e) ? 1
      : 0;
    setFlipDir(dir);
    if (dir !== 0) {
      setDropIndex(null);
      return;
    }
    if (e.clientY < 70) {
      window.scrollBy({ top: -14 });
    } else if (e.clientY > window.innerHeight - 70) {
      window.scrollBy({ top: 14 });
    }
    updateDropIndex(e.clientY);
  }

  async function commitReorder() {
    const id = dragId;
    const to = dropIndex;
    const origin = dragOrigin.current;
    resetDrag();
    if (id === null || to === null || origin === null) return;
    const from = tracks.findIndex((t) => t.id === id);
    const origGlobal = from !== -1 ? (loadedPage - 1) * PER_PAGE + from : origin.globalIndex;
    const targetGlobal = (loadedPage - 1) * PER_PAGE + to;
    if (targetGlobal === origGlobal || targetGlobal === origGlobal + 1) return;
    const newIndex = targetGlobal > origGlobal ? targetGlobal - 1 : targetGlobal;

    const moved = from !== -1 ? tracks[from] : origin.track;
    const next = tracks.filter((t) => t.id !== id);
    next.splice(from !== -1 && from < to ? to - 1 : to, 0, moved);
    setTracks(next.slice(0, PER_PAGE));

    try {
      await reorderTrack(id, newIndex, onUnauthorized, viewPlaylist);
    } catch (e) {
      showToast(`${t("common.error")}: ${(e as Error).message}`);
    } finally {
      setLocalVersion((v) => v + 1);
    }
  }

  async function deleteTrack(trackId: string) {
    try {
      await authOk(
        `/api/tracks/${encodeURIComponent(trackId)}`,
        "history.deleteFailed",
        { method: "DELETE" },
        onUnauthorized,
      );
      onTrackDeleted(trackId);
      setLocalVersion((v) => v + 1);
      showToast(t("history.trackDeleted"));
    } catch (e) {
      showToast(`${t("common.error")}: ${(e as Error).message}`);
    }
  }

  async function removeTrackFromView(trackId: string) {
    if (!viewPlaylist) return;
    try {
      await removeFromPlaylist(viewPlaylist, trackId, onUnauthorized);
      setLocalVersion((v) => v + 1);
      showToast(t("history.removedFromPlaylist"));
    } catch (e) {
      showToast(`${t("common.error")}: ${(e as Error).message}`);
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
      showToast(`${t("history.playlistCreated")}: ${playlist.name}`);
      onPlaylistCreated(playlist);
      switchView(playlist.id);
    } catch (e) {
      showToast(`${t("common.error")}: ${(e as Error).message}`);
    }
  }

  async function submitRename() {
    const name = (renameName ?? "").trim();
    if (!name || !viewingPlaylist) {
      setRenameName(null);
      return;
    }
    try {
      await renamePlaylist(viewingPlaylist.id, name, onUnauthorized);
      setRenameName(null);
      showToast(t("history.playlistRenamed"));
    } catch (e) {
      showToast(`${t("common.error")}: ${(e as Error).message}`);
    }
  }

  async function deleteViewingPlaylist() {
    if (!viewingPlaylist) return;
    try {
      await deletePlaylist(viewingPlaylist.id, onUnauthorized);
      showToast(`${t("history.playlistDeleted")}: ${viewingPlaylist.name}`);
      switchView(null);
    } catch (e) {
      showToast(`${t("common.error")}: ${(e as Error).message}`);
    }
  }

  function toggleSelect(trackId: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(trackId)) next.delete(trackId);
      else next.add(trackId);
      return next;
    });
  }

  function selectAllOnPage() {
    setSelected((prev) => {
      const pageIds = tracks.map((t) => t.id);
      const allSelected = pageIds.every((id) => prev.has(id));
      const next = new Set(prev);
      if (allSelected) {
        pageIds.forEach((id) => next.delete(id));
      } else {
        pageIds.forEach((id) => next.add(id));
      }
      return next;
    });
  }

  async function bulkDelete() {
    if (selected.size === 0) return;
    try {
      const { deleted } = await bulkDeleteTracks(Array.from(selected), onUnauthorized);
      for (const id of selected) onTrackDeleted(id);
      exitSelectMode();
      setLocalVersion((v) => v + 1);
      showToast(`${deleted} ${t("history.tracksDeleted")}`);
    } catch (e) {
      showToast(`${t("common.error")}: ${(e as Error).message}`);
    }
  }

  async function bulkAddToPlaylistAction(playlistId: string) {
    if (selected.size === 0) return;
    setBulkMenuOpen(false);
    try {
      const data = await bulkAddToPlaylist(playlistId, Array.from(selected), onUnauthorized);
      showToast(data.message || t("history.addedToPlaylist"));
      exitSelectMode();
    } catch (e) {
      showToast(`${t("common.error")}: ${(e as Error).message}`);
    }
  }

  async function addTrackToPlaylist(playlistId: string, trackId: string) {
    setMenuTrackId(null);
    try {
      const data = await addToPlaylist(playlistId, trackId, onUnauthorized);
      showToast(data.message || t("history.addedToPlaylist"));
    } catch (e) {
      showToast(`${t("common.error")}: ${(e as Error).message}`);
    }
  }

  return (
    <div className="history-section">
      <div className="playlist-bar">
        <button
          className={`playlist-tab${viewPlaylist === null ? " active" : ""}`}
          onClick={() => switchView(null)}
        >
          {t("history.library")}
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
            title={t("history.createPlaylist")}
            onClick={() => setNewName("")}
          >
            ＋
          </button>
        ) : (
          <span className="playlist-new">
            <input
              type="text"
              className="playlist-new-input"
              placeholder={t("history.playlistName")}
              autoFocus
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") submitNewPlaylist();
                if (e.key === "Escape") setNewName(null);
              }}
            />
            <button className="btn btn-sm" onClick={submitNewPlaylist}>
              {t("history.create")}
            </button>
            <button className="text-btn" onClick={() => setNewName(null)}>
              {t("history.cancel")}
            </button>
          </span>
        )}
      </div>

      <div className="section-label history-header">
        {viewingPlaylist && renameName !== null ? (
          <span className="playlist-new">
            <input
              type="text"
              className="playlist-new-input"
              autoFocus
              value={renameName}
              onChange={(e) => setRenameName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") submitRename();
                if (e.key === "Escape") setRenameName(null);
              }}
            />
            <button className="btn btn-sm" onClick={submitRename}>
              {t("history.rename")}
            </button>
            <button className="text-btn" onClick={() => setRenameName(null)}>
              {t("history.cancel")}
            </button>
          </span>
        ) : (
          <>
            <span
              onClick={() => { if (viewingPlaylist) setRenameName(viewingPlaylist.name); }}
              title={viewingPlaylist ? t("history.renamePlaylist") : undefined}
              style={viewingPlaylist ? { cursor: "pointer" } : undefined}
            >
              {viewingPlaylist
                ? `${viewingPlaylist.name} (${total})`
                : `${t("history.tracks")} (${total})`}
            </span>
            {viewingPlaylist && (
              <button
                className="text-btn text-btn-danger"
                onClick={deleteViewingPlaylist}
              >
                {t("history.deletePlaylist")}
              </button>
            )}
          </>
        )}
      </div>

      <div className="history-toolbar">
        <input
          type="text"
          className="history-filter"
          placeholder={t("history.filterPlaceholder")}
          value={filterInput}
          onChange={(e) => handleFilterChange(e.target.value)}
        />
        {total > 0 && (
          <button
            className={`btn btn-outline btn-sm${selectMode ? " active" : ""}`}
            onClick={() => { if (selectMode) exitSelectMode(); else setSelectMode(true); }}
          >
            {selectMode ? t("history.cancelSelect") : t("history.selectMode")}
          </button>
        )}
      </div>

      {selectMode && tracks.length > 0 && (
        <div className="bulk-actions">
          <button className="text-btn" onClick={selectAllOnPage}>
            {tracks.every((tr) => selected.has(tr.id))
              ? t("history.deselectAll")
              : t("history.selectAll")}
          </button>
          <span className="bulk-count">{selected.size}</span>
          <button
            className="btn btn-sm"
            disabled={selected.size === 0}
            onClick={bulkDelete}
          >
            {t("history.bulkDelete")}
          </button>
          {!viewPlaylist && playlists.length > 0 && (
            <span className="playlist-menu-anchor">
              <button
                className="btn btn-outline btn-sm"
                disabled={selected.size === 0}
                onClick={() => setBulkMenuOpen(!bulkMenuOpen)}
              >
                {t("history.bulkAddToPlaylist")}
              </button>
              {bulkMenuOpen && (
                <AddToPlaylistMenu
                  playlists={playlists}
                  onAdd={(pid) => bulkAddToPlaylistAction(pid)}
                  onClose={() => setBulkMenuOpen(false)}
                />
              )}
            </span>
          )}
        </div>
      )}

      {total === 0 && (
        <div className="history-empty">
          {viewPlaylist
            ? t("history.playlistEmpty")
            : t("history.noTracks")}
        </div>
      )}

      <div
        className="history-list"
        ref={listRef}
        onPointerMove={handleDragMove}
        onPointerUp={() => commitReorder()}
        onPointerCancel={resetDrag}
      >
        {tracks.map((tr, i) => {
          const isCurrent = currentTrack?.id === tr.id;
          const isSelected = selected.has(tr.id);
          const classes = ["history-item"];
          if (dragId === tr.id) classes.push("dragging");
          if (dropIndex === i) classes.push("drop-before");
          if (i === tracks.length - 1 && dropIndex === tracks.length) {
            classes.push("drop-after");
          }
          if (selectMode && isSelected) classes.push("selected");
          return (
            <div
              key={tr.id}
              className={classes.join(" ")}
              style={isCurrent && !selectMode ? { borderColor: "var(--accent)" } : undefined}
              onClick={() => selectMode ? toggleSelect(tr.id) : onSelectTrack(tr)}
            >
              {selectMode ? (
                <span className="select-check">
                  <span className={`select-check-box${isSelected ? " checked" : ""}`}>
                    {isSelected && <span className="select-check-mark" />}
                  </span>
                </span>
              ) : total > 1 && !filter && (
                <span
                  className="drag-handle"
                  title={t("history.dragToReorder")}
                  onClick={(e) => e.stopPropagation()}
                  onPointerDown={(e) => handleDragStart(e, tr, i)}
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
              <TrackRowInfo track={tr} />
              {!viewPlaylist && (
                <span className="playlist-menu-anchor" onClick={(e) => e.stopPropagation()}>
                  <button
                    className="delete-btn add-btn"
                    title={t("history.addToPlaylist")}
                    onClick={() => setMenuTrackId(menuTrackId === tr.id ? null : tr.id)}
                  >
                    <AddToListIcon />
                  </button>
                  {menuTrackId === tr.id && (
                    <AddToPlaylistMenu
                      playlists={playlists}
                      onAdd={(pid) => addTrackToPlaylist(pid, tr.id)}
                      onClose={() => setMenuTrackId(null)}
                    />
                  )}
                </span>
              )}
              <button
                className="delete-btn"
                title={viewPlaylist ? t("history.removeFromPlaylist") : t("history.deleteTrack")}
                onClick={(e) => {
                  e.stopPropagation();
                  if (viewPlaylist) removeTrackFromView(tr.id);
                  else deleteTrack(tr.id);
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
            {t("history.prev")}
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
            {t("history.next")}
          </button>
        </div>
      )}
    </div>
  );
}
