import type { PlaybackMode } from "../types";

const MODES: { value: PlaybackMode; label: string; hint: string }[] = [
  { value: "loop", label: "ループ", hint: "ライブラリ順に連続再生" },
  { value: "shuffle", label: "シャッフル", hint: "ランダムに連続再生" },
  { value: "off", label: "オフ", hint: "曲が終わったら停止" },
];

interface Props {
  mode: PlaybackMode;
  onChange: (mode: PlaybackMode) => void;
}

export function PlaybackModeSelector({ mode, onChange }: Props) {
  return (
    <div className="playback-mode-section">
      <div className="section-label">連続再生</div>
      <div className="segmented">
        {MODES.map((m) => (
          <button
            key={m.value}
            className={`segmented-btn${mode === m.value ? " active" : ""}`}
            title={m.hint}
            onClick={() => onChange(m.value)}
          >
            {m.label}
          </button>
        ))}
      </div>
    </div>
  );
}
