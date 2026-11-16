import { useEffect, useRef, useCallback, useState } from "react";
import type { Dispatch, SetStateAction } from "react";
import { getToken, reorderPlaylist, reorderTrack, PER_PAGE } from "./api";
import type { Device, DownloadProgress, PlaybackMode, Playlist, Track, WSMessage } from "./types";

interface WSCallbacks {
  onVersion: (version: string) => void;
  /** Track-list revision at connect time (undefined if the server omits it) */
  onInitTracks: (rev: number | undefined) => void;
  onDeviceUpdate: (devices: Record<string, Device>) => void;
  onTracksUpdate: () => void;
  onPlaybackMode: (mode: PlaybackMode) => void;
  onExtractResult: (track: Track) => void;
  onExtractError: (error: string) => void;
  onDownloadsUpdate: (downloads: DownloadProgress[]) => void;
  onPlaylistsUpdate: (playlists: Playlist[]) => void;
  onActivePlaylist: (playlistId: string | null) => void;
  onSleepTimer: (expiresAt: number | null) => void;
  onPlaylistImportStarted: (name: string, total: number) => void;
  onConnectedChange: (connected: boolean) => void;
}

// Reconnect backoff, doubling per consecutive failure.
const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 30000;

/** A malformed frame is reported and skipped, not worth tearing the connection down. */
function parseMessage(raw: string): WSMessage | null {
  try {
    return JSON.parse(raw);
  } catch {
    console.warn("Ignoring malformed WebSocket message");
    return null;
  }
}

export function useWebSocket(active: boolean, callbacks: WSCallbacks) {
  const wsRef = useRef<WebSocket | null>(null);
  const keepAliveRef = useRef<ReturnType<typeof setInterval>>(undefined);
  const reconnectRef = useRef<ReturnType<typeof setTimeout>>(undefined);
  const retriesRef = useRef(0);
  const cbRef = useRef(callbacks);
  cbRef.current = callbacks;

  const connect = useCallback(() => {
    const protocol = location.protocol === "https:" ? "wss:" : "ws:";
    let wsUrl = `${protocol}//${location.host}/ws`;
    const token = getToken();
    if (token) wsUrl += `?token=${encodeURIComponent(token)}`;

    const ws = new WebSocket(wsUrl);
    wsRef.current = ws;

    ws.onopen = () => {
      retriesRef.current = 0;
      cbRef.current.onConnectedChange(true);
    };

    ws.onclose = () => {
      cbRef.current.onConnectedChange(false);
      const delay = Math.min(
        RECONNECT_BASE_MS * 2 ** retriesRef.current,
        RECONNECT_MAX_MS,
      );
      retriesRef.current += 1;
      reconnectRef.current = setTimeout(connect, delay);
    };

    ws.onerror = () => {
      ws.close();
    };

    ws.onmessage = (event) => {
      const data = parseMessage(event.data);
      if (!data) return;
      const cb = cbRef.current;
      switch (data.type) {
        case "init":
          if (data.version) cb.onVersion(data.version);
          cb.onDeviceUpdate(data.devices || {});
          cb.onInitTracks(data.tracks_rev);
          if (data.playback_mode) cb.onPlaybackMode(data.playback_mode);
          // Restores the progress display after a reload or reconnect
          cb.onDownloadsUpdate(data.downloads || []);
          cb.onPlaylistsUpdate(data.playlists || []);
          cb.onActivePlaylist(data.active_playlist ?? null);
          cb.onSleepTimer(data.sleep_timer ?? null);
          break;
        case "device_update":
          cb.onDeviceUpdate(data.devices || {});
          break;
        case "tracks_update":
          cb.onTracksUpdate();
          break;
        case "playback_mode_update":
          cb.onPlaybackMode(data.mode);
          break;
        case "extract_audio_result":
          cb.onExtractResult(data.track);
          break;
        case "extract_audio_error":
          cb.onExtractError(data.error);
          break;
        case "downloads_update":
          cb.onDownloadsUpdate(data.downloads || []);
          break;
        case "playlists_update":
          cb.onPlaylistsUpdate(data.playlists || []);
          break;
        case "active_playlist_update":
          cb.onActivePlaylist(data.playlist ?? null);
          break;
        case "sleep_timer_update":
          cb.onSleepTimer(data.expires_at ?? null);
          break;
        case "playlist_import_result":
          cb.onPlaylistImportStarted(data.name, data.total);
          break;
        // extract_audio_cancelled is deliberately silent: Stop all already
        // clears the display.
      }
    };

    if (keepAliveRef.current) clearInterval(keepAliveRef.current);
    keepAliveRef.current = setInterval(() => {
      if (ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "ping" }));
      }
    }, 30000);
  }, []);

  const sendMessage = useCallback((msg: Record<string, unknown>): boolean => {
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      wsRef.current.send(JSON.stringify(msg));
      return true;
    }
    return false;
  }, []);

  useEffect(() => {
    if (!active) return;
    connect();
    return () => {
      if (reconnectRef.current) clearTimeout(reconnectRef.current);
      if (keepAliveRef.current) clearInterval(keepAliveRef.current);
      if (wsRef.current) {
        wsRef.current.onclose = null;
        wsRef.current.close();
      }
    };
  }, [active, connect]);

  return { sendMessage };
}

// How long a drag held over a pagination button waits before turning the page,
// and how far from the viewport edge it starts scrolling (and by how much).
const PAGE_FLIP_MS = 650;
const EDGE_SCROLL_MARGIN = 70;
const EDGE_SCROLL_STEP = 14;

/**
 * The loaded page after `moved` is dropped into slot `to`. `from` is its row on
 * this page, or -1 when it came from another page.
 */
function previewMove(
  tracks: Track[],
  from: number,
  to: number,
  moved: Track,
  fromEarlierPage: boolean,
): Track[] {
  if (from !== -1) {
    const next = tracks.filter((_, i) => i !== from);
    next.splice(from < to ? to - 1 : to, 0, tracks[from]);
    return next;
  }
  if (fromEarlierPage) {
    // The first row is pushed back; slot 0 lands on the previous page.
    if (to === 0) return tracks;
    return [...tracks.slice(1, to), moved, ...tracks.slice(to)];
  }
  // From a later page the last row is pushed on.
  return [...tracks.slice(0, to), moved, ...tracks.slice(to)].slice(0, tracks.length);
}

/**
 * The index an item at `from` ends up at when dropped into slot `to` (slots
 * counted with it still in place), or null when either side of itself.
 */
function slotToIndex(from: number, to: number): number | null {
  if (to === from || to === from + 1) return null;
  return to > from ? to - 1 : to;
}

interface ReorderOptions {
  /** The page on screen, and the page the loaded tracks were fetched for (they differ mid-fetch). */
  page: number;
  loadedPage: number;
  totalPages: number;
  setPage: Dispatch<SetStateAction<number>>;
  tracks: Track[];
  setTracks: Dispatch<SetStateAction<Track[]>>;
  /** False for a single track or a filtered view, which does not show the stored order. */
  enabled: boolean;
  /** Playlist whose order is being edited (null reorders the whole library). */
  playlistId: string | null;
  onUnauthorized: () => void;
  onError: (error: unknown) => void;
  /** Run once the move has been persisted or has failed. */
  onSettled: () => void;
}

/** Drag-to-reorder for a paginated track list, including edge scrolling and page flips. */
export function useTrackReorder(opts: ReorderOptions) {
  const { page, loadedPage, totalPages, setPage, tracks, setTracks, enabled } = opts;
  const [dragId, setDragId] = useState<string | null>(null);
  const [dropIndex, setDropIndex] = useState<number | null>(null);
  /**
   * Where the drag began. Kept because the row can leave the loaded page
   * mid-drag (a page flip), and the move is still relative to where it started.
   */
  const dragOrigin = useRef<{ track: Track; globalIndex: number } | null>(null);
  /** Direction the held-over pagination button is flipping in, 0 for neither. */
  const [flipDir, setFlipDir] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);
  const prevBtnRef = useRef<HTMLButtonElement>(null);
  const nextBtnRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (flipDir === 0) return;
    if (flipDir === -1 ? page <= 1 : page >= totalPages) {
      setFlipDir(0);
      return;
    }
    const timer = window.setInterval(() => setPage((p) => p + flipDir), PAGE_FLIP_MS);
    return () => clearInterval(timer);
  }, [flipDir, page, totalPages, setPage]);

  function reset() {
    setFlipDir(0);
    dragOrigin.current = null;
    setDragId(null);
    setDropIndex(null);
  }

  /**
   * The row index the drop would land before, from the pointer's position
   * relative to each row's midpoint. Past the last midpoint it is the end.
   */
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

  function isOver(el: HTMLElement | null, e: React.PointerEvent) {
    if (!el) return false;
    const r = el.getBoundingClientRect();
    return e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom;
  }

  function handleDragStart(e: React.PointerEvent<HTMLElement>, track: Track, index: number) {
    if (!enabled) return;
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
    // While a page flip is armed there is no drop slot: the rows under the
    // pointer are about to be replaced by another page's.
    if (dir !== 0) {
      setDropIndex(null);
      return;
    }
    if (e.clientY < EDGE_SCROLL_MARGIN) {
      window.scrollBy({ top: -EDGE_SCROLL_STEP });
    } else if (e.clientY > window.innerHeight - EDGE_SCROLL_MARGIN) {
      window.scrollBy({ top: EDGE_SCROLL_STEP });
    }
    updateDropIndex(e.clientY);
  }

  /** Reorder the visible rows at once, then persist and let the caller re-fetch. */
  async function commit() {
    const id = dragId;
    const to = dropIndex;
    const origin = dragOrigin.current;
    reset();
    if (id === null || to === null || origin === null) return;
    const pageStart = (loadedPage - 1) * PER_PAGE;
    const from = tracks.findIndex((t) => t.id === id);
    const origGlobal = from !== -1 ? pageStart + from : origin.globalIndex;
    const targetGlobal = pageStart + to;
    const newIndex = slotToIndex(origGlobal, targetGlobal);
    if (newIndex === null) return;

    setTracks(previewMove(tracks, from, to, origin.track, origGlobal < pageStart));

    try {
      await reorderTrack(id, newIndex, opts.onUnauthorized, opts.playlistId);
    } catch (e) {
      opts.onError(e);
    } finally {
      opts.onSettled();
    }
  }

  return {
    listRef,
    prevBtnRef,
    nextBtnRef,
    dragId,
    dropIndex,
    flipDir,
    handleDragStart,
    handleDragMove,
    commit,
    reset,
  };
}

// Mouse travel (px) / touch hold (ms) that turns a press into a tab drag.
const TAB_DRAG_THRESHOLD = 6;
const TAB_LONG_PRESS_MS = 400;

interface TabReorderOptions {
  onUnauthorized: () => void;
  onError: (error: unknown) => void;
}

/**
 * Drag-to-reorder for the playlist tabs (`data-playlist-id` buttons inside
 * `barRef`). The click that ends a drag is swallowed.
 */
export function usePlaylistTabReorder(playlists: Playlist[], opts: TabReorderOptions) {
  // Shown until a broadcast after the move arrives.
  const [pending, setPending] = useState<string[] | null>(null);
  const settled = useRef(false);
  const [dragId, setDragId] = useState<string | null>(null);
  const [dropIndex, setDropIndex] = useState<number | null>(null);
  const barRef = useRef<HTMLDivElement>(null);
  const press = useRef<{
    id: string;
    pointerId: number;
    x: number;
    y: number;
    timer?: number;
    dragging?: boolean;
  } | null>(null);
  const swallowClick = useRef(false);

  useEffect(() => {
    if (settled.current) setPending(null);
  }, [playlists]);

  const byId = new Map(playlists.map((p) => [p.id, p]));
  const ordered = pending
    ? pending.flatMap((id) => byId.get(id) ?? [])
    : playlists;

  // Non-passive, so a touch drag can block page scroll.
  useEffect(() => {
    const bar = barRef.current;
    if (!bar) return;
    const onTouchMove = (e: TouchEvent) => {
      if (press.current?.dragging) e.preventDefault();
    };
    bar.addEventListener("touchmove", onTouchMove, { passive: false });
    return () => bar.removeEventListener("touchmove", onTouchMove);
  }, []);

  function reset() {
    clearTimeout(press.current?.timer);
    press.current = null;
    setDragId(null);
    setDropIndex(null);
  }

  /** Drop slot: after every tab on an earlier row, or left of the pointer on its row. */
  function updateDropIndex(x: number, y: number) {
    const tabs = barRef.current?.querySelectorAll<HTMLElement>("[data-playlist-id]");
    if (!tabs) return;
    let idx = 0;
    tabs.forEach((el, i) => {
      const r = el.getBoundingClientRect();
      if (y > r.bottom || (y >= r.top && x > r.left + r.width / 2)) idx = i + 1;
    });
    setDropIndex(idx);
  }

  function begin(x: number, y: number) {
    const p = press.current;
    if (!p) return;
    p.dragging = true;
    try {
      barRef.current?.setPointerCapture(p.pointerId);
    } catch {
      // Pointer already released.
    }
    setDragId(p.id);
    updateDropIndex(x, y);
  }

  function handlePointerDown(e: React.PointerEvent<HTMLElement>) {
    swallowClick.current = false;
    const id = (e.target as Element).closest<HTMLElement>("[data-playlist-id]")?.dataset.playlistId;
    if (!id || playlists.length < 2 || e.button !== 0) return;
    reset();
    const p: NonNullable<typeof press.current> = { id, pointerId: e.pointerId, x: e.clientX, y: e.clientY };
    press.current = p;
    if (e.pointerType !== "mouse") {
      p.timer = window.setTimeout(() => begin(p.x, p.y), TAB_LONG_PRESS_MS);
    }
  }

  function handlePointerMove(e: React.PointerEvent<HTMLElement>) {
    const p = press.current;
    if (!p || p.pointerId !== e.pointerId) return;
    // Released outside the bar.
    if (e.pointerType === "mouse" && e.buttons === 0) return reset();
    if (p.dragging) return updateDropIndex(e.clientX, e.clientY);
    if (Math.hypot(e.clientX - p.x, e.clientY - p.y) <= TAB_DRAG_THRESHOLD) return;
    // A touch that moves before the hold completes is a scroll.
    if (e.pointerType === "mouse") begin(e.clientX, e.clientY);
    else reset();
  }

  async function handlePointerUp(e: React.PointerEvent<HTMLElement>) {
    const p = press.current;
    if (!p || p.pointerId !== e.pointerId) return;
    const to = dropIndex;
    reset();
    if (!p.dragging) return;
    swallowClick.current = true;
    const ids = ordered.map((pl) => pl.id);
    const from = ids.indexOf(p.id);
    const newIndex = to === null || from === -1 ? null : slotToIndex(from, to);
    if (newIndex === null) return;
    ids.splice(from, 1);
    ids.splice(newIndex, 0, p.id);
    settled.current = false;
    setPending(ids);
    try {
      await reorderPlaylist(p.id, newIndex, opts.onUnauthorized);
    } catch (err) {
      setPending(null);
      opts.onError(err);
    } finally {
      settled.current = true;
    }
  }

  function handleClickCapture(e: React.MouseEvent) {
    if (!swallowClick.current) return;
    swallowClick.current = false;
    e.preventDefault();
    e.stopPropagation();
  }

  function handleContextMenu(e: React.MouseEvent) {
    if (dragId) e.preventDefault();
  }

  return {
    barRef,
    ordered,
    dragId,
    dropIndex,
    handlePointerDown,
    handlePointerMove,
    handlePointerUp,
    handlePointerCancel: reset,
    handleClickCapture,
    handleContextMenu,
  };
}
