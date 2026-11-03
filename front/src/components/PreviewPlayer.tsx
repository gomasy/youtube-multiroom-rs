import { useEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { getStreamUrl, relayUrl } from "../api";
import { showApiError } from "../errors";
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
  const requestRef = useRef<AbortController>(null);
  /** The signed URL, once loaded */
  const srcRef = useRef<string>(null);
  /** A relay cannot seek in place: a seek reopens it, and its clock counts from here (secs) */
  const baseRef = useRef(0);
  /** Debounces reopening, so a slider drag opens one relay */
  const reopenRef = useRef<ReturnType<typeof setTimeout>>(null);
  const relayed = !!track.streamed;
  const [playing, setPlaying] = useState(false);
  const [loading, setLoading] = useState(false);
  const [position, setPosition] = useState(0);
  const durationSec = track.is_live ? 0 : (track.duration ?? 0);
  const seekable = durationSec > 0;

  useEffect(() => {
    const audio = audioRef.current;
    return () => {
      requestRef.current?.abort();
      if (reopenRef.current) clearTimeout(reopenRef.current);
      audio?.pause();
      audio?.removeAttribute("src");
      audio?.load();
    };
  }, []);

  async function toggle() {
    const audio = audioRef.current;
    if (!audio || loading) return;
    if (playing) {
      audio.pause();
      return;
    }
    try {
      if (!srcRef.current) {
        const request = new AbortController();
        requestRef.current = request;
        setLoading(true);
        const src = await getStreamUrl(track.id, onUnauthorized, request.signal);
        if (request.signal.aborted) return;
        srcRef.current = src;
        openAt(audio, 0);
      }
      await audio.play();
    } catch (e) {
      showApiError(showToast, e);
    } finally {
      if (!requestRef.current?.signal.aborted) setLoading(false);
    }
  }

  function openAt(audio: HTMLAudioElement, positionSec: number) {
    baseRef.current = positionSec;
    audio.src = relayUrl(srcRef.current!, positionSec);
  }

  function seek(positionSec: number) {
    const audio = audioRef.current;
    if (!audio || !srcRef.current) return;
    if (relayed) {
      if (reopenRef.current) clearTimeout(reopenRef.current);
      reopenRef.current = setTimeout(() => {
        const wasPlaying = !audio.paused;
        openAt(audio, positionSec);
        if (wasPlaying) void audio.play().catch(() => {});
      }, 300);
    } else {
      audio.currentTime = positionSec;
    }
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
        onEnded={(e) => {
          setPlaying(false);
          setPosition(0);
          // Otherwise the next play would resume from the last seek
          if (relayed && baseRef.current > 0) openAt(e.target as HTMLAudioElement, 0);
        }}
        onTimeUpdate={(e) =>
          setPosition(baseRef.current + (e.target as HTMLAudioElement).currentTime)}
        onError={() => {
          srcRef.current = null;
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
