import { useEffect, useRef, useCallback } from "react";
import { getToken } from "./api";
import type { Device, Track, WSMessage } from "./types";

interface WSCallbacks {
  onInit: (devices: Record<string, Device>, tracks: Record<string, Track>) => void;
  onDeviceUpdate: (devices: Record<string, Device>) => void;
  onTracksUpdate: (tracks: Record<string, Track>) => void;
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
        cbRef.current.onInit(data.devices || {}, data.tracks || {});
      } else if (data.type === "device_update") {
        cbRef.current.onDeviceUpdate(data.devices || {});
      } else if (data.type === "tracks_update") {
        cbRef.current.onTracksUpdate(data.tracks || {});
      }
    };

    if (keepAliveRef.current) clearInterval(keepAliveRef.current);
    keepAliveRef.current = setInterval(() => {
      if (ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "ping" }));
      }
    }, 30000);
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
}
