import { useEffect, useRef, useCallback, useState } from "react";
import type { Dispatch, SetStateAction } from "react";
import { getToken, reorderTrack, PER_PAGE } from "./api";
import type { Device, DownloadProgress, PlaybackMode, Playlist, Track, WSMessage } from "./types";

interface WSCallbacks {
  onVersion: (version: string) => void;
  onInit: (devices: Record<string, Device>) => void;
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

// Reconnect backoff, doubling per consecutive failure so a server that stays
// down is not hammered once every 3 seconds by every open tab.
const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 30000;

/**
 * A frame we cannot parse is not worth tearing the connection down for, so
 * report it and let the socket carry on.
 */
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
          cb.onInit(data.devices || {});
          // The REST snapshot predates this subscription, so the gap is
          // reconciled here rather than waited out.
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
        // clears the display for every job it stopped, so a toast per cancelled
        // request would only report what the user just asked for.
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

interface ReorderOptions {
  /**
   * The page on screen, and the page the loaded tracks were fetched for. These
   * differ between a page change and its fetch landing, and every index here is
   * global (page offset + row), so both are needed.
   */
  page: number;
  loadedPage: number;
  totalPages: number;
  setPage: Dispatch<SetStateAction<number>>;
  tracks: Track[];
  setTracks: Dispatch<SetStateAction<Track[]>>;
  /**
   * Whether there is an order to edit. A single track has nothing to move, and
   * a filtered view is not showing the stored order, so a drop position in it
   * would name the wrong slot.
   */
  enabled: boolean;
  /** Playlist whose order is being edited (null reorders the whole library). */
  playlistId: string | null;
  onUnauthorized: () => void;
  onError: (error: unknown) => void;
  /**
   * Run once the move has been persisted or has failed, so the caller can
   * re-fetch and replace the optimistic order with what the server stored.
   */
  onSettled: () => void;
}

/**
 * Drag-to-reorder for a paginated track list.
 *
 * Owns the pointer gesture end to end: the drop position, edge scrolling,
 * holding over a pagination button to turn the page, and the optimistic
 * reorder written back on release. The caller supplies the list state and gets
 * back the refs to attach and the indices that drive the row styling.
 */
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

  /**
   * Apply the move where the pointer was released: reorder the visible rows at
   * once so the list does not snap back mid-request, then persist and let the
   * caller re-fetch the authoritative order.
   */
  async function commit() {
    const id = dragId;
    const to = dropIndex;
    const origin = dragOrigin.current;
    reset();
    if (id === null || to === null || origin === null) return;
    const from = tracks.findIndex((t) => t.id === id);
    const origGlobal = from !== -1 ? (loadedPage - 1) * PER_PAGE + from : origin.globalIndex;
    const targetGlobal = (loadedPage - 1) * PER_PAGE + to;
    // Dropping onto either side of where the row already sits is not a move.
    if (targetGlobal === origGlobal || targetGlobal === origGlobal + 1) return;
    // The drop slot is counted with the row still in place, so a move downwards
    // shifts by one once it has been lifted out.
    const newIndex = targetGlobal > origGlobal ? targetGlobal - 1 : targetGlobal;

    const moved = from !== -1 ? tracks[from] : origin.track;
    const next = tracks.filter((t) => t.id !== id);
    next.splice(from !== -1 && from < to ? to - 1 : to, 0, moved);
    setTracks(next.slice(0, PER_PAGE));

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
