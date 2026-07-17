import { useState, useCallback, useEffect, useRef } from "react";
import { checkAuth } from "./api";
import { useWebSocket } from "./hooks";
import { Header } from "./components/Header";
import { UrlInput } from "./components/UrlInput";
import type { UrlInputHandle } from "./components/UrlInput";
import { NowPlaying } from "./components/NowPlaying";
import { DownloadList } from "./components/DownloadList";
import { DeviceList } from "./components/DeviceList";
import { PlaybackModeSelector } from "./components/PlaybackModeSelector";
import { History } from "./components/History";
import { AuthModal } from "./components/AuthModal";
import { ToastContainer, useToast } from "./components/Toast";
import type { Device, DownloadProgress, PlaybackMode, Track, TracksPage } from "./types";

export function App() {
  const [showAuth, setShowAuth] = useState(false);
  const [wsActive, setWsActive] = useState(false);
  const [connected, setConnected] = useState(false);
  const [version, setVersion] = useState<string | null>(null);
  const [devices, setDevices] = useState<Record<string, Device>>({});
  const [tracksVersion, setTracksVersion] = useState(0);
  // 認証確認時に取得した 1 ページ目のスナップショット。History が一度だけ消費する
  const [initialTracks, setInitialTracks] = useState<TracksPage | null>(null);
  const [currentTrack, setCurrentTrack] = useState<Track | null>(null);
  const [playbackMode, setPlaybackMode] = useState<PlaybackMode>("off");
  const [downloads, setDownloads] = useState<DownloadProgress[]>([]);
  const { toasts, showToast } = useToast();
  const urlInputRef = useRef<UrlInputHandle>(null);

  const [extracting, setExtracting] = useState(false);
  const onUnauthorized = useCallback(() => setShowAuth(true), []);

  useEffect(() => {
    checkAuth().then(({ authorized, data }) => {
      if (!authorized) {
        setShowAuth(true);
      } else {
        setInitialTracks(data);
        setWsActive(true);
      }
    });
  }, []);

  const handleExtractResult = useCallback((track: Track) => {
    setExtracting(false);
    setCurrentTrack(track);
    showToast(`「${track.title}」を取得しました`);
    urlInputRef.current?.clear();
  }, [showToast]);

  const handleExtractError = useCallback((error: string) => {
    setExtracting(false);
    showToast(`エラー: ${error}`);
  }, [showToast]);

  const { sendMessage } = useWebSocket(wsActive, {
    onVersion: setVersion,
    onConnectedChange: (c) => {
      setConnected(c);
      if (!c) setExtracting(false);
    },
    onInit: setDevices,
    onDeviceUpdate: setDevices,
    onTracksUpdate: () => setTracksVersion((v) => v + 1),
    onPlaybackMode: setPlaybackMode,
    onExtractResult: handleExtractResult,
    onExtractError: handleExtractError,
    onDownloadsUpdate: setDownloads,
  });

  function handleTrackDeleted(trackId: string) {
    if (currentTrack?.id === trackId) setCurrentTrack(null);
  }

  function handleDeviceDeleted(deviceId: string) {
    setDevices((prev) => {
      const next = { ...prev };
      delete next[deviceId];
      return next;
    });
  }

  function handlePlaybackModeChange(mode: PlaybackMode) {
    // 表示の更新は保存成功時にサーバーが返す playback_mode_update に任せる
    if (!sendMessage({ type: "set_playback_mode", mode })) {
      showToast("サーバーに接続されていません");
    }
  }

  function handleAuthenticated(data: TracksPage | null) {
    setShowAuth(false);
    if (data) setInitialTracks(data);
    setWsActive(true);
  }

  return (
    <>
      <div className="app">
        <Header connected={connected} version={version} />
        <UrlInput
          ref={urlInputRef}
          extracting={extracting}
          onUnauthorized={onUnauthorized}
          onExtract={(url) => {
            if (sendMessage({ type: "extract_audio", url })) {
              setExtracting(true);
            } else {
              showToast("サーバーに接続されていません");
            }
          }}
          showToast={showToast}
        />
        <DownloadList downloads={downloads} />
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
            <PlaybackModeSelector
              mode={playbackMode}
              onChange={handlePlaybackModeChange}
            />
          </div>
          <div className="main-right">
            <History
              active={wsActive}
              initialData={initialTracks}
              refreshKey={tracksVersion}
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
