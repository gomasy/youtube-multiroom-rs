import { useEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { getStreamUrl } from "../api";
import { formatTime } from "../format";
import { PauseIcon, PlayIcon } from "./icons";
import type { Track } from "../types";

interface Props {
  track: Track;
  onUnauthorized: () => void;
  showToast: (msg: string) => void;
}

/**
 * 選択中トラックをブラウザで試聴するプレーヤー。Echo へ送る前の内容確認用。
 * 呼び出し側が key={track.id} を付けるので、トラックが変わるとコンポーネント
 * ごと作り直され、再生状態のリセットはアンマウントに任せられる。
 * ストリーム URL (署名付き) は初回再生時に取得する
 */
export function PreviewPlayer({ track, onUnauthorized, showToast }: Props) {
  const audioRef = useRef<HTMLAudioElement>(null);
  // ストリーム URL を audio に設定済みか。再生失敗時は取得し直せるよう戻す
  const srcLoadedRef = useRef(false);
  const [playing, setPlaying] = useState(false);
  const [loading, setLoading] = useState(false);
  const [position, setPosition] = useState(0); // 秒
  // ライブ配信は長さ不明 (Infinity) になるためシーク不可として扱う
  const durationSec = track.is_live ? 0 : (track.duration ?? 0);
  const seekable = durationSec > 0;

  // DOM から外れた audio 要素は再生が止まらないため、アンマウントで確実に止める
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
      showToast(`エラー: ${(e as Error).message}`);
    } finally {
      setLoading(false);
    }
  }

  function seek(positionSec: number) {
    const audio = audioRef.current;
    // まだ読み込んでいない状態のシークは位置表示だけ動かしても仕方がないので無視
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
        // 再生終了では pause イベントが発火しないブラウザもあるため両方戻す
        onEnded={() => {
          setPlaying(false);
          setPosition(0);
        }}
        onTimeUpdate={(e) => setPosition((e.target as HTMLAudioElement).currentTime)}
        onError={() => {
          // 失敗した URL は捨て、次の再生ボタンで取得し直す
          // (署名の期限切れやキャッシュ削除からの復帰のため)
          srcLoadedRef.current = false;
          setPlaying(false);
          setLoading(false);
          showToast("試聴の再生に失敗しました");
        }}
      />
      <button
        className="preview-btn"
        onClick={toggle}
        disabled={loading}
        title={playing ? "試聴を一時停止" : "ブラウザで試聴"}
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
            aria-label="試聴位置"
            style={{ "--seek-pct": `${pct}%` } as CSSProperties}
            onChange={(e) => seek(Number(e.target.value))}
          />
          <span className="seek-time">{formatTime(durationSec)}</span>
        </div>
      ) : (
        <span className="preview-hint">
          {playing || loading ? formatTime(position) : "ブラウザで試聴"}
        </span>
      )}
    </div>
  );
}
