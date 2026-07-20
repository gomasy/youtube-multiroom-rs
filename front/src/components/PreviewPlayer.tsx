import { useEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { getStreamUrl } from "../api";
import { formatTime } from "../format";
import { t } from "../i18n";
import { PauseIcon, PlayIcon } from "./icons";
import type { Track } from "../types";

interface Props {
  track: Track;
  onUnauthorized: () => void;
  showToast: (msg: string) => void;
}

export function PreviewPlayer({ track, onUnauthorized, showToast }: Props) {
  const audioRef = useRef<HTMLAudioElement>(null);
  const srcLoadedRef = useRef(false);
  const [playing, setPlaying] = useState(false);
  const [loading, setLoading] = useState(false);
  const [position, setPosition] = useState(0);
  const durationSec = track.is_live ? 0 : (track.duration ?? 0);
  const seekable = durationSec > 0;

  useEffect(() => {
    const audio = audioRef.current;
    return () => audio?.pause();
  }, []);

  async function toggle() {
    const audio = audioRef.current;
    if (!audio || loading) return;
    if (playing) {
      audio.pause();
      return;
    }
    try {
      if (!srcLoadedRef.current) {
        setLoading(true);
        audio.src = await getStreamUrl(track.id, onUnauthorized);
        srcLoadedRef.current = true;
      }
      await audio.play();
    } catch (e) {
      showToast(`${t("common.error")}: ${(e as Error).message}`);
    } finally {
      setLoading(false);
    }
  }

  function seek(positionSec: number) {
    const audio = audioRef.current;
    if (!audio || !srcLoadedRef.current) return;
    audio.currentTime = positionSec;
    setPosition(positionSec);
  }

  const pct = seekable ? (position / durationSec) * 100 : 0;

  return (
    <div className="preview-player">
      <audio
        ref={audioRef}
        preload="none"
        onPlay={() => setPlaying(true)}
        onPause={() => setPlaying(false)}
        onEnded={() => {
          setPlaying(false);
          setPosition(0);
        }}
        onTimeUpdate={(e) => setPosition((e.target as HTMLAudioElement).currentTime)}
        onError={() => {
          srcLoadedRef.current = false;
          setPlaying(false);
          setLoading(false);
          showToast(t("preview.playbackFailed"));
        }}
      />
      <button
        className="preview-btn"
        onClick={toggle}
        disabled={loading}
        title={playing ? t("preview.pause") : t("preview.play")}
      >
        {loading ? <span className="spinner" /> : playing ? <PauseIcon /> : <PlayIcon />}
      </button>
      {seekable ? (
        <div className="seek-bar">
          <span className="seek-time">{formatTime(position)}</span>
          <input
            type="range"
            min={0}
            max={durationSec}
            step={1}
            value={Math.min(position, durationSec)}
            aria-label={t("preview.position")}
            style={{ "--seek-pct": `${pct}%` } as CSSProperties}
            onChange={(e) => seek(Number(e.target.value))}
          />
          <span className="seek-time">{formatTime(durationSec)}</span>
        </div>
      ) : (
        <span className="preview-hint">
          {playing || loading ? formatTime(position) : t("preview.play")}
        </span>
      )}
    </div>
  );
}
