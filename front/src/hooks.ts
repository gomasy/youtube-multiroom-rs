import { useEffect, useRef, useCallback } from "react";
import { getToken } from "./api";
import type { Device, DownloadProgress, PlaybackMode, Playlist, Track, WSMessage } from "./types";

interface WSCallbacks {
  onVersion: (version: string) => void;
  onInit: (devices: Record<string, Device>) => void;
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

export function useWebSocket(active: boolean, callbacks: WSCallbacks) {
  const wsRef = useRef<WebSocket | null>(null);
  const keepAliveRef = useRef<ReturnType<typeof setInterval>>(undefined);
  const reconnectRef = useRef<ReturnType<typeof setTimeout>>(undefined);
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
      cbRef.current.onConnectedChange(true);
    };

    ws.onclose = () => {
      cbRef.current.onConnectedChange(false);
      reconnectRef.current = setTimeout(connect, 3000);
    };

    ws.onerror = () => {
      ws.close();
    };

    ws.onmessage = (event) => {
      const data: WSMessage = JSON.parse(event.data);
      if (data.type === "init") {
        if (data.version) cbRef.current.onVersion(data.version);
        cbRef.current.onInit(data.devices || {});
        if (data.playback_mode) cbRef.current.onPlaybackMode(data.playback_mode);
        // Re-sync in-progress download display on reload/reconnect
        cbRef.current.onDownloadsUpdate(data.downloads || []);
        cbRef.current.onPlaylistsUpdate(data.playlists || []);
        cbRef.current.onActivePlaylist(data.active_playlist ?? null);
        cbRef.current.onSleepTimer(data.sleep_timer ?? null);
      } else if (data.type === "device_update") {
        cbRef.current.onDeviceUpdate(data.devices || {});
      } else if (data.type === "tracks_update") {
        cbRef.current.onTracksUpdate();
      } else if (data.type === "playback_mode_update") {
        cbRef.current.onPlaybackMode(data.mode);
      } else if (data.type === "extract_audio_result") {
        cbRef.current.onExtractResult(data.track);
      } else if (data.type === "extract_audio_error") {
        cbRef.current.onExtractError(data.error);
      } else if (data.type === "downloads_update") {
        cbRef.current.onDownloadsUpdate(data.downloads || []);
      } else if (data.type === "playlists_update") {
        cbRef.current.onPlaylistsUpdate(data.playlists || []);
      } else if (data.type === "active_playlist_update") {
        cbRef.current.onActivePlaylist(data.playlist ?? null);
      } else if (data.type === "sleep_timer_update") {
        cbRef.current.onSleepTimer(data.expires_at ?? null);
      } else if (data.type === "playlist_import_result") {
        cbRef.current.onPlaylistImportStarted(data.name, data.total);
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
