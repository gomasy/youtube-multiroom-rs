import { useState, useCallback, useEffect, useRef } from "react";
import { checkAuth } from "./api";
import { useWebSocket } from "./hooks";
import { Header } from "./components/Header";
import { UrlInput } from "./components/UrlInput";
import type { UrlInputHandle } from "./components/UrlInput";
import { NowPlaying } from "./components/NowPlaying";
import { DeviceList } from "./components/DeviceList";
import { History } from "./components/History";
import { AuthModal } from "./components/AuthModal";
import { ToastContainer, useToast } from "./components/Toast";
import type { Device, Track } from "./types";

export function App() {
  const [showAuth, setShowAuth] = useState(false);
  const [wsActive, setWsActive] = useState(false);
  const [connected, setConnected] = useState(false);
  const [devices, setDevices] = useState<Record<string, Device>>({});
  const [tracks, setTracks] = useState<Record<string, Track>>({});
  const [currentTrack, setCurrentTrack] = useState<Track | null>(null);
  const { toasts, showToast } = useToast();
  const urlInputRef = useRef<UrlInputHandle>(null);

  const [extracting, setExtracting] = useState(false);
  const onUnauthorized = useCallback(() => setShowAuth(true), []);

  useEffect(() => {
    checkAuth().then((ok) => {
      if (!ok) {
        setShowAuth(true);
      } else {
        setWsActive(true);
      }
    });
  }, []);

  const handleExtractResult = useCallback((track: Track) => {
    setExtracting(false);
    setCurrentTrack(track);
    setTracks((prev) => ({ ...prev, [track.id]: track }));
    showToast(`「${track.title}」を取得しました`);
    urlInputRef.current?.clear();
  }, [showToast]);

  const handleExtractError = useCallback((error: string) => {
    setExtracting(false);
    showToast(`エラー: ${error}`);
  }, [showToast]);

  const { sendMessage } = useWebSocket(wsActive, {
    onConnectedChange: (c) => {
      setConnected(c);
      if (!c) setExtracting(false);
    },
    onInit: (devs, trks) => {
      setDevices(devs);
      setTracks(trks);
    },
    onDeviceUpdate: setDevices,
    onTracksUpdate: (trks) => {
      setTracks(trks);
      setCurrentTrack((prev) => (prev && !(prev.id in trks) ? null : prev));
    },
    onExtractResult: handleExtractResult,
    onExtractError: handleExtractError,
  });

  function handleTrackDeleted(trackId: string) {
    setTracks((prev) => {
      const next = { ...prev };
      delete next[trackId];
      return next;
    });
    if (currentTrack?.id === trackId) setCurrentTrack(null);
  }

  function handleDeviceDeleted(deviceId: string) {
    setDevices((prev) => {
      const next = { ...prev };
      delete next[deviceId];
      return next;
    });
  }

  function handleAuthenticated() {
    setShowAuth(false);
    setWsActive(true);
  }

  return (
    <>
      <div className="app">
        <Header connected={connected} />
        <UrlInput
          ref={urlInputRef}
          extracting={extracting}
          onExtract={(url) => {
            if (sendMessage({ type: "extract_audio", url })) {
              setExtracting(true);
            } else {
              showToast("サーバーに接続されていません");
            }
          }}
          showToast={showToast}
        />
        <div className="main-grid">
          <div className="main-left">
            <NowPlaying track={currentTrack} />
            <DeviceList
              devices={devices}
              currentTrack={currentTrack}
              onDeviceDeleted={handleDeviceDeleted}
              onUnauthorized={onUnauthorized}
              showToast={showToast}
            />
          </div>
          <div className="main-right">
            <History
              tracks={tracks}
              currentTrack={currentTrack}
              onSelectTrack={setCurrentTrack}
              onTrackDeleted={handleTrackDeleted}
              onUnauthorized={onUnauthorized}
              showToast={showToast}
            />
          </div>
        </div>
      </div>

      {showAuth && (
        <AuthModal onAuthenticated={handleAuthenticated} showToast={showToast} />
      )}

      <ToastContainer toasts={toasts} />
    </>
  );
}
