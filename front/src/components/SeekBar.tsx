import { useEffect, useState } from "react";
import type { CSSProperties } from "react";
import { formatTime } from "../format";
import type { Device } from "../types";

/** 最後に受信した位置 + 経過時間から現在の再生位置 (ミリ秒) を推定する */
function estimatePosition(device: Device, durationMs: number): number {
  const elapsed =
    device.status === "playing" && device.last_update
      ? Math.max(Date.now() - device.last_update * 1000, 0)
      : 0;
  return Math.min((device.position_ms ?? 0) + elapsed, durationMs);
}

interface Props {
  device: Device;
  onSeek: (positionMs: number) => void;
}

/** デバイスの再生位置バー。シーク不可 (トラックなし・ライブ・長さ不明) なら何も描画しない */
export function SeekBar({ device, onSeek }: Props) {
  // ドラッグ中はサーバー由来の推定位置ではなくつまみの位置を表示する
  const [dragValue, setDragValue] = useState<number | null>(null);
  const [, setTick] = useState(0);

  // 再生中は 1 秒ごとに再描画して推定位置を進める
  useEffect(() => {
    if (device.status !== "playing") return;
    const id = setInterval(() => setTick((t) => t + 1), 1000);
    return () => clearInterval(id);
  }, [device.status]);

  const track = device.current_track;
  const durationMs = (track?.duration ?? 0) * 1000;
  if (!track || track.is_live || durationMs <= 0) return null;

  const position = dragValue ?? estimatePosition(device, durationMs);
  const pct = (position / durationMs) * 100;

  function commit() {
    if (dragValue !== null) {
      onSeek(dragValue);
      setDragValue(null);
    }
  }

  return (
    // デバイスカード内に置かれるため、操作がカードの選択切り替えに化けないようにする
    <div className="seek-bar" onClick={(e) => e.stopPropagation()}>
      <span className="seek-time">{formatTime(position / 1000)}</span>
      <input
        type="range"
        min={0}
        max={durationMs}
        step={1000}
        value={position}
        aria-label="再生位置"
        style={{ "--seek-pct": `${pct}%` } as CSSProperties}
        onChange={(e) => setDragValue(Number(e.target.value))}
        onPointerDown={(e) => e.stopPropagation()}
        onPointerUp={commit}
        onPointerCancel={() => setDragValue(null)}
        onKeyUp={commit}
      />
      <span className="seek-time">{formatTime(durationMs / 1000)}</span>
    </div>
  );
}
