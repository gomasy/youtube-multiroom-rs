import { useEffect, useRef, useCallback } from "react";
import { getToken } from "./api";
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
  onExtractCancelled: () => void;
  onDownloadsUpdate: (downloads: DownloadProgress[]) => void;
  onPlaylistsUpdate: (playlists: Playlist[]) => void;
  onActivePlaylist: (playlistId: string | null) => void;
  onSleepTimer: (expiresAt: number | null) => void;
  onPlaylistImportStarted: (name: string, total: number) => void;
  onConnectedChange: (connected: boolean) => void;
}

/// Reconnect backoff. Doubles per consecutive failure so a server that stays
/// down is not hammered once every 3 seconds by every open tab.
const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 30000;

/// A frame we cannot parse is not worth tearing the connection down for, so
/// report it and let the socket carry on.
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
          // The REST snapshot is fetched before this subscription exists, so
          // the gap has to be reconciled here rather than waited out.
          cb.onInitTracks(data.tracks_rev);
          if (data.playback_mode) cb.onPlaybackMode(data.playback_mode);
          // Re-sync in-progress download display on reload/reconnect
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
        case "extract_audio_cancelled":
          cb.onExtractCancelled();
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
