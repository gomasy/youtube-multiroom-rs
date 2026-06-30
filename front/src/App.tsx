import { useState, useCallback, useEffect } from "react";
import { checkAuth } from "./api";
import { useWebSocket } from "./hooks";
import { Header } from "./components/Header";
import { UrlInput } from "./components/UrlInput";
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

  useWebSocket(wsActive, {
    onConnectedChange: setConnected,
    onInit: (devs, trks) => {
      setDevices(devs);
      setTracks(trks);
    },
    onDeviceUpdate: setDevices,
    onTracksUpdate: (trks) => {
      setTracks(trks);
      setCurrentTrack((prev) => (prev && !(prev.id in trks) ? null : prev));
    },
  });

  function handleTrackExtracted(track: Track) {
    setCurrentTrack(track);
    setTracks((prev) => ({ ...prev, [track.id]: track }));
  }

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
          onTrackExtracted={handleTrackExtracted}
          onUnauthorized={onUnauthorized}
          showToast={showToast}
        />
        <NowPlaying track={currentTrack} />
        <DeviceList
          devices={devices}
          currentTrack={currentTrack}
          onDeviceDeleted={handleDeviceDeleted}
          onUnauthorized={onUnauthorized}
          showToast={showToast}
        />
        <History
          tracks={tracks}
          currentTrack={currentTrack}
          onSelectTrack={setCurrentTrack}
          onTrackDeleted={handleTrackDeleted}
          onUnauthorized={onUnauthorized}
          showToast={showToast}
        />
      </div>

      {showAuth && (
        <AuthModal onAuthenticated={handleAuthenticated} showToast={showToast} />
      )}

      <ToastContainer toasts={toasts} />
    </>
  );
}
