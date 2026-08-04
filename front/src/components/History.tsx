import { useEffect, useRef, useState } from "react";
import {
  addToPlaylist,
  authOk,
  bulkAddToPlaylist,
  bulkDeleteTracks,
  bulkRemoveFromPlaylist,
  createPlaylist,
  deletePlaylist,
  fetchTracks,
  refreshTracksMetadata,
  removeFromPlaylist,
  renamePlaylist,
  PER_PAGE,
} from "../api";
import { showApiError } from "../errors";
import { useTrackReorder } from "../hooks";
import { t, tFmt } from "../i18n";
import { TrackRowInfo } from "./TrackRowInfo";
import { AddToPlaylistMenu } from "./AddToPlaylistMenu";
import { AddToListIcon, CloseIcon, GripIcon, TrashIcon } from "./icons";
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
  const viewPlaylistRef = useRef<string | null>(null);
  viewPlaylistRef.current = viewPlaylist;
  const [newName, setNewName] = useState<string | null>(null);
  const [menuTrackId, setMenuTrackId] = useState<string | null>(null);
  const [localVersion, setLocalVersion] = useState(0);
  const consumedInitial = useRef<TracksPage | null>(null);
  const [loadedPage, setLoadedPage] = useState(1);
  const [filterInput, setFilterInput] = useState("");
  const [filter, setFilter] = useState("");
  const filterTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const [selectMode, setSelectMode] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [bulkMenuOpen, setBulkMenuOpen] = useState(false);
  const [renameName, setRenameName] = useState<string | null>(null);

  const totalPages = lastPage(total);
  const viewingPlaylist = playlists.find((p) => p.id === viewPlaylist) ?? null;
  const reorder = useTrackReorder({
    page,
    loadedPage,
    totalPages,
    setPage,
    tracks,
    setTracks,
    // A filtered view is not showing the stored order, so a drop position in it
    // would name the wrong slot.
    enabled: total > 1 && !filter,
    playlistId: viewPlaylist,
    onUnauthorized,
    onError: (e) => showApiError(showToast, e),
    onSettled: () => setLocalVersion((v) => v + 1),
  });

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
      .catch((error: unknown) => {
        if (cancelled) return;
        showError(error);
      });
    return () => {
      cancelled = true;
    };
  }, [active, initialData, page, refreshKey, localVersion, viewPlaylist, filter, onUnauthorized]);

  if (total === 0 && !filter && !viewPlaylist && playlists.length === 0) return null;

  function showError(e: unknown) {
    showApiError(showToast, e);
  }

  function renderNameForm(
    value: string,
    onChange: (v: string) => void,
    onSubmit: () => void,
    onCancel: () => void,
    submitLabel: string,
    placeholder?: string,
  ) {
    return (
      <span className="playlist-new">
        <input
          type="text"
          className="input playlist-new-input"
          placeholder={placeholder}
          autoFocus
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") onSubmit();
            if (e.key === "Escape") onCancel();
          }}
        />
        <button className="btn btn-sm" onClick={onSubmit}>
          {submitLabel}
        </button>
        <button className="text-btn" onClick={onCancel}>
          {t("history.cancel")}
        </button>
      </span>
    );
  }

  function exitSelectMode() {
    setSelectMode(false);
    setSelected(new Set());
    setBulkMenuOpen(false);
  }

  function switchView(playlistId: string | null) {
    setViewPlaylist(playlistId);
    setPage(1);
    setTracks([]);
    setTotal(0);
    setMenuTrackId(null);
    setRenameName(null);
    clearTimeout(filterTimer.current);
    setFilterInput("");
    setFilter("");
    exitSelectMode();
    reorder.reset();
  }

  function handleFilterChange(value: string) {
    setFilterInput(value);
    clearTimeout(filterTimer.current);
    filterTimer.current = setTimeout(() => {
      setFilter(value.trim());
      setPage(1);
    }, 300);
  }

  async function deleteTrack(track: Track) {
    if (!window.confirm(tFmt("history.confirmDeleteTrack", { title: track.title }))) return;
    try {
      await authOk(
        `/api/tracks/${encodeURIComponent(track.id)}`,
        "history.deleteFailed",
        { method: "DELETE" },
        onUnauthorized,
      );
      onTrackDeleted(track.id);
      setLocalVersion((v) => v + 1);
      showToast(t("history.trackDeleted"));
    } catch (e) {
      showError(e);
    }
  }

  async function removeTrackFromView(track: Track) {
    if (!viewPlaylist || !viewingPlaylist) return;
    if (!window.confirm(tFmt("history.confirmRemoveFromPlaylist", {
      title: track.title,
      playlist: viewingPlaylist.name,
    }))) return;
    try {
      await removeFromPlaylist(viewPlaylist, track.id, onUnauthorized);
      setLocalVersion((v) => v + 1);
      showToast(t("history.removedFromPlaylist"));
    } catch (e) {
      showError(e);
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
      showError(e);
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
      showError(e);
    }
  }

  async function deleteViewingPlaylist() {
    if (!viewingPlaylist) return;
    const playlist = viewingPlaylist;
    if (!window.confirm(tFmt("history.confirmDeletePlaylist", { name: playlist.name }))) return;
    try {
      await deletePlaylist(playlist.id, onUnauthorized);
      showToast(`${t("history.playlistDeleted")}: ${playlist.name}`);
      if (viewPlaylistRef.current === playlist.id) switchView(null);
    } catch (e) {
      showError(e);
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

  /**
   * Removal from the open playlist, or permanent deletion from the library when
   * none is open. The two are never interchangeable, so the view is resolved
   * once up front and re-checked after every await — those give the user time
   * to switch views, and acting on the new one would touch the wrong tracks.
   */
  async function bulkRemove() {
    if (selected.size === 0) return;
    const trackIds = Array.from(selected);
    const playlist = viewingPlaylist;
    // A playlist is open but its metadata is gone (deleted concurrently). Do
    // nothing rather than fall through to deleting from the library.
    if (viewPlaylist !== null && playlist === null) return;

    const confirmed = playlist
      ? window.confirm(tFmt("history.confirmBulkRemoveFromPlaylist", {
          count: trackIds.length,
          playlist: playlist.name,
        }))
      : window.confirm(tFmt("history.confirmBulkDelete", { count: trackIds.length }));
    if (!confirmed) return;

    try {
      if (playlist) {
        const { removed } = await bulkRemoveFromPlaylist(playlist.id, trackIds, onUnauthorized);
        showToast(tFmt("history.tracksRemovedFromPlaylist", { count: removed }));
        if (viewPlaylistRef.current !== playlist.id) return;
      } else {
        const { deleted } = await bulkDeleteTracks(trackIds, onUnauthorized);
        for (const id of trackIds) onTrackDeleted(id);
        showToast(`${deleted} ${t("history.tracksDeleted")}`);
        if (viewPlaylistRef.current !== null) return;
      }
      exitSelectMode();
      setLocalVersion((v) => v + 1);
    } catch (e) {
      showError(e);
    }
  }

  /**
   * Unlike the other bulk actions, not scoped to the open view: metadata
   * belongs to the track itself, so refreshing from inside a playlist updates
   * the same library entries the library view would.
   */
  async function bulkRefreshMetadata() {
    if (selected.size === 0) return;
    try {
      const data = await refreshTracksMetadata(Array.from(selected), onUnauthorized);
      showToast(data.message || tFmt("history.metadataRefreshStarted", { count: data.total }));
      exitSelectMode();
    } catch (e) {
      showError(e);
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
      showError(e);
    }
  }

  async function addTrackToPlaylist(playlistId: string, trackId: string) {
    setMenuTrackId(null);
    try {
      const data = await addToPlaylist(playlistId, trackId, onUnauthorized);
      showToast(data.message || t("history.addedToPlaylist"));
    } catch (e) {
      showError(e);
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
        ) : renderNameForm(
          newName,
          setNewName,
          submitNewPlaylist,
          () => setNewName(null),
          t("history.create"),
          t("history.playlistName"),
        )}
      </div>

      <div className="section-label history-header">
        {viewingPlaylist && renameName !== null ? renderNameForm(
          renameName,
          setRenameName,
          submitRename,
          () => setRenameName(null),
          t("history.rename"),
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
          className="input history-filter"
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
            onClick={bulkRemove}
          >
            {viewPlaylist ? t("history.bulkRemoveFromPlaylist") : t("history.bulkDelete")}
          </button>
          <button
            className="btn btn-outline btn-sm"
            title={t("history.bulkRefreshMetadataHint")}
            disabled={selected.size === 0}
            onClick={bulkRefreshMetadata}
          >
            {t("history.bulkRefreshMetadata")}
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
        ref={reorder.listRef}
        onPointerMove={reorder.handleDragMove}
        onPointerUp={() => reorder.commit()}
        onPointerCancel={reorder.reset}
      >
        {tracks.map((tr, i) => {
          const isCurrent = currentTrack?.id === tr.id;
          const isSelected = selected.has(tr.id);
          const classes = ["history-item"];
          if (reorder.dragId === tr.id) classes.push("dragging");
          if (reorder.dropIndex === i) classes.push("drop-before");
          if (i === tracks.length - 1 && reorder.dropIndex === tracks.length) {
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
                  onPointerDown={(e) => reorder.handleDragStart(e, tr, i)}
                >
                  <GripIcon />
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
                  if (viewPlaylist) removeTrackFromView(tr);
                  else deleteTrack(tr);
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
            ref={reorder.prevBtnRef}
            className={"btn btn-outline btn-sm" + (reorder.flipDir === -1 ? " drag-over" : "")}
            disabled={page <= 1}
            onClick={() => setPage(page - 1)}
          >
            {t("history.prev")}
          </button>
          <span className="pagination-info">
            {page} / {totalPages}
          </span>
          <button
            ref={reorder.nextBtnRef}
            className={"btn btn-outline btn-sm" + (reorder.flipDir === 1 ? " drag-over" : "")}
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
