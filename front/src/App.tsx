import { useState, useCallback, useEffect, useRef } from "react";
import { checkAuth } from "./api";
import { showApiError } from "./errors";
import { t, tFmt } from "./i18n";
import { useWebSocket } from "./hooks";
import { Header } from "./components/Header";
import { UrlInput } from "./components/UrlInput";
import { NowPlaying } from "./components/NowPlaying";
import { DownloadList } from "./components/DownloadList";
import { DeviceList } from "./components/DeviceList";
import { PlaybackModeSelector } from "./components/PlaybackModeSelector";
import { History } from "./components/History";
import { AuthModal } from "./components/AuthModal";
import { ToastContainer, useToast } from "./components/Toast";
import { SleepTimer } from "./components/SleepTimer";
import { LibraryTools } from "./components/LibraryTools";
import type { Device, DownloadProgress, PlaybackMode, Playlist, Track, TracksPage } from "./types";

export function App() {
  const [showAuth, setShowAuth] = useState(false);
  const [wsActive, setWsActive] = useState(false);
  const [connected, setConnected] = useState(false);
  const [version, setVersion] = useState<string | null>(null);
  const [devices, setDevices] = useState<Record<string, Device>>({});
  const [tracksVersion, setTracksVersion] = useState(0);
  const [initialTracks, setInitialTracks] = useState<TracksPage | null>(null);
  /**
   * Whether an init frame has already been handled on this page load. Only the
   * first one can be answered by the REST snapshot; later ones are reconnects.
   */
  const seenInit = useRef(false);
  const [currentTrack, setCurrentTrack] = useState<Track | null>(null);
  const [playbackMode, setPlaybackMode] = useState<PlaybackMode>("off");
  const [downloads, setDownloads] = useState<DownloadProgress[]>([]);
  const [playlists, setPlaylists] = useState<Playlist[]>([]);
  const [activePlaylist, setActivePlaylist] = useState<string | null>(null);
  const [sleepTimer, setSleepTimer] = useState<number | null>(null);
  const { toasts, showToast } = useToast();

  const onUnauthorized = useCallback(() => {
    setWsActive(false);
    setConnected(false);
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
    setCurrentTrack(track);
    showToast(`${t("common.trackFetched")}: ${track.title}`);
  }, [showToast]);

  const handleExtractError = useCallback((error: string) => {
    showToast(`${t("common.error")}: ${error}`);
  }, [showToast]);

  const handlePlaylistImportStarted = useCallback((name: string, total: number) => {
    showToast(`${name}: ${tFmt("common.importStarted", { total })}`);
  }, [showToast]);

  /**
   * Drop the REST snapshot and re-fetch the visible page. The snapshot only
   * describes the state before the first render, so once it is known to be
   * behind it must not be applied again.
   */
  const refreshTracks = useCallback(() => {
    setInitialTracks(null);
    setTracksVersion((v) => v + 1);
  }, []);

  const handlePlaylistCreated = useCallback((playlist: Playlist) => {
    setPlaylists((prev) =>
      prev.some((p) => p.id === playlist.id) ? prev : [...prev, playlist],
    );
  }, []);

  const { sendMessage } = useWebSocket(wsActive, {
    onVersion: setVersion,
    onConnectedChange: setConnected,
    onInit: setDevices,
    onInitTracks: (rev) => {
      // The REST snapshot was served before this subscription existed. A
      // matching revision means nothing changed in that gap, so the page on
      // screen is current. Anything else — a changed revision, a server too old
      // to report one, or a reconnect, whose gap is the whole disconnected
      // period and whose counter a restart may have reset — needs a fetch.
      const snapshotIsCurrent =
        !seenInit.current && initialTracks !== null && initialTracks.rev === rev;
      seenInit.current = true;
      if (!snapshotIsCurrent) refreshTracks();
    },
    onDeviceUpdate: setDevices,
    onTracksUpdate: refreshTracks,
    onPlaybackMode: setPlaybackMode,
    onExtractResult: handleExtractResult,
    onExtractError: handleExtractError,
    onDownloadsUpdate: setDownloads,
    onPlaylistsUpdate: setPlaylists,
    onActivePlaylist: setActivePlaylist,
    onSleepTimer: setSleepTimer,
    onPlaylistImportStarted: handlePlaylistImportStarted,
  });

  /**
   * Push a command to the server, reporting the one way it fails here: the
   * socket is down, so nothing was sent. Returns whether it went out, for
   * callers that also have local state to flip.
   */
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
    if (data) {
      // A snapshot fetched just before the socket opens can answer the init
      // frame exactly as the one from the initial page load does.
      setInitialTracks(data);
      seenInit.current = false;
    }
    setWsActive(true);
  }

  return (
    <>
      <div className="app">
        <Header connected={connected} version={version} />
        <UrlInput
          onUnauthorized={onUnauthorized}
          onExtract={(url) => send({ type: "extract_audio", url })}
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
            <LibraryTools onUnauthorized={onUnauthorized} showToast={showToast} />
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
