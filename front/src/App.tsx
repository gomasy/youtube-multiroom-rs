import { useState, useCallback, useEffect, useRef } from "react";
import { checkAuth } from "./api";
import { showApiError } from "./errors";
import { t, tFmt } from "./i18n";
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
import { SleepTimer } from "./components/SleepTimer";
import type { Device, DownloadProgress, PlaybackMode, Playlist, Track, TracksPage } from "./types";

export function App() {
  const [showAuth, setShowAuth] = useState(false);
  const [wsActive, setWsActive] = useState(false);
  const [connected, setConnected] = useState(false);
  const [version, setVersion] = useState<string | null>(null);
  const [devices, setDevices] = useState<Record<string, Device>>({});
  const [tracksVersion, setTracksVersion] = useState(0);
  const [initialTracks, setInitialTracks] = useState<TracksPage | null>(null);
  const [currentTrack, setCurrentTrack] = useState<Track | null>(null);
  const [playbackMode, setPlaybackMode] = useState<PlaybackMode>("off");
  const [downloads, setDownloads] = useState<DownloadProgress[]>([]);
  const [playlists, setPlaylists] = useState<Playlist[]>([]);
  const [activePlaylist, setActivePlaylist] = useState<string | null>(null);
  const [sleepTimer, setSleepTimer] = useState<number | null>(null);
  const { toasts, showToast } = useToast();
  const urlInputRef = useRef<UrlInputHandle>(null);

  const [extracting, setExtracting] = useState(false);
  const onUnauthorized = useCallback(() => {
    setWsActive(false);
    setConnected(false);
    setExtracting(false);
    setShowAuth(true);
  }, []);

  useEffect(() => {
    let cancelled = false;
    let retryTimer: number | undefined;
    let errorReported = false;

    function verifyAuth() {
      void checkAuth()
        .then(({ authorized, data }) => {
          if (cancelled) return;
          if (!authorized) {
            setShowAuth(true);
          } else {
            setInitialTracks(data);
            setWsActive(true);
          }
        })
        .catch((error: unknown) => {
          if (cancelled) return;
          if (!errorReported) {
            showApiError(showToast, error);
            errorReported = true;
          }
          retryTimer = window.setTimeout(verifyAuth, 3000);
        });
    }

    verifyAuth();
    return () => {
      cancelled = true;
      if (retryTimer !== undefined) clearTimeout(retryTimer);
    };
  }, [showToast]);

  const handleExtractResult = useCallback((track: Track) => {
    setExtracting(false);
    setCurrentTrack(track);
    showToast(`${t("common.trackFetched")}: ${track.title}`);
    urlInputRef.current?.clear();
  }, [showToast]);

  const handleExtractError = useCallback((error: string) => {
    setExtracting(false);
    showToast(`${t("common.error")}: ${error}`);
  }, [showToast]);

  const handlePlaylistImportStarted = useCallback((name: string, total: number) => {
    setExtracting(false);
    showToast(`${name}: ${tFmt("common.importStarted", { total })}`);
    urlInputRef.current?.clear();
  }, [showToast]);

  const handlePlaylistCreated = useCallback((playlist: Playlist) => {
    setPlaylists((prev) =>
      prev.some((p) => p.id === playlist.id) ? prev : [...prev, playlist],
    );
  }, []);

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
    onExtractCancelled: () => setExtracting(false),
    onDownloadsUpdate: setDownloads,
    onPlaylistsUpdate: setPlaylists,
    onActivePlaylist: setActivePlaylist,
    onSleepTimer: setSleepTimer,
    onPlaylistImportStarted: handlePlaylistImportStarted,
  });

  /// Push a command to the server, reporting the one way it can fail here: the
  /// socket is down, so the command was never sent. Returns whether it went out,
  /// for callers that also have local state to flip.
  function send(msg: Record<string, unknown>): boolean {
    if (sendMessage(msg)) return true;
    showToast(t("common.notConnected"));
    return false;
  }

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
    send({ type: "set_playback_mode", mode });
  }

  function handleActivePlaylistChange(playlistId: string | null) {
    send({ type: "set_active_playlist", playlist: playlistId });
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
            if (send({ type: "extract_audio", url })) setExtracting(true);
          }}
          showToast={showToast}
        />
        <DownloadList
          downloads={downloads}
          onCancel={() => send({ type: "cancel_downloads" })}
        />
        <div className="main-grid">
          <div className="main-left">
            <NowPlaying
              track={currentTrack}
              onUnauthorized={onUnauthorized}
              showToast={showToast}
            />
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
              playlists={playlists}
              activePlaylist={activePlaylist}
              onActivePlaylistChange={handleActivePlaylistChange}
            />
            <SleepTimer
              expiresAt={sleepTimer}
              onSet={(minutes) => send({ type: "set_sleep_timer", minutes })}
              onCancel={() => send({ type: "set_sleep_timer", minutes: null })}
            />
          </div>
          <div className="main-right">
            <History
              active={wsActive}
              initialData={initialTracks}
              refreshKey={tracksVersion}
              currentTrack={currentTrack}
              playlists={playlists}
              onPlaylistCreated={handlePlaylistCreated}
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
