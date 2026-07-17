import { useEffect, useRef, useCallback } from "react";
import { getToken } from "./api";
import type { Device, DownloadProgress, PlaybackMode, Track, WSMessage } from "./types";

interface WSCallbacks {
  onInit: (devices: Record<string, Device>) => void;
  onDeviceUpdate: (devices: Record<string, Device>) => void;
  onTracksUpdate: () => void;
  onPlaybackMode: (mode: PlaybackMode) => void;
  onExtractResult: (track: Track) => void;
  onExtractError: (error: string) => void;
  onDownloadsUpdate: (downloads: DownloadProgress[]) => void;
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
        cbRef.current.onInit(data.devices || {});
        if (data.playback_mode) cbRef.current.onPlaybackMode(data.playback_mode);
        // リロード・再接続時に進行中ダウンロードの表示を同期し直す
        cbRef.current.onDownloadsUpdate(data.downloads || []);
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
