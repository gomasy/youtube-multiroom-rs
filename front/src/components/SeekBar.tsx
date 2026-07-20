import { useEffect, useState } from "react";
import type { CSSProperties } from "react";
import { formatTime } from "../format";
import { t } from "../i18n";
import type { Device } from "../types";

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

export function SeekBar({ device, onSeek }: Props) {
  const [dragValue, setDragValue] = useState<number | null>(null);
  const [, setTick] = useState(0);

  const track = device.current_track;
  const durationMs = (track?.duration ?? 0) * 1000;
  const seekable = !!track && !track.is_live && durationMs > 0;

  useEffect(() => {
    if (device.status !== "playing" || !seekable) return;
    const id = setInterval(() => setTick((t) => t + 1), 1000);
    return () => clearInterval(id);
  }, [device.status, seekable]);

  if (!seekable) return null;

  const position = dragValue ?? estimatePosition(device, durationMs);
  const pct = (position / durationMs) * 100;

  function commit() {
    if (dragValue !== null) {
      onSeek(dragValue);
      setDragValue(null);
    }
  }

  return (
    <div className="seek-bar" onClick={(e) => e.stopPropagation()}>
      <span className="seek-time">{formatTime(position / 1000)}</span>
      <input
        type="range"
        min={0}
        max={durationMs}
        step={1000}
        value={position}
        aria-label={t("seek.position")}
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
