import { t } from "../i18n";
import type { PlaybackMode, Playlist } from "../types";

const MODES: { value: PlaybackMode; labelKey: string; hintKey: string }[] = [
  { value: "off", labelKey: "mode.off", hintKey: "mode.off.hint" },
  { value: "loop", labelKey: "mode.loop", hintKey: "mode.loop.hint" },
  { value: "shuffle", labelKey: "mode.shuffle", hintKey: "mode.shuffle.hint" },
];

interface Props {
  mode: PlaybackMode;
  onChange: (mode: PlaybackMode) => void;
  playlists: Playlist[];
  activePlaylist: string | null;
  onActivePlaylistChange: (playlistId: string | null) => void;
}

export function PlaybackModeSelector({
  mode,
  onChange,
  playlists,
  activePlaylist,
  onActivePlaylistChange,
}: Props) {
  return (
    <div className="playback-mode-section">
      <div className="section-label">{t("mode.label")}</div>
      <div className="segmented">
        {MODES.map((m) => (
          <button
            key={m.value}
            className={`segmented-btn${mode === m.value ? " active" : ""}`}
            title={t(m.hintKey)}
            onClick={() => onChange(m.value)}
          >
            {t(m.labelKey)}
          </button>
        ))}
      </div>
      {playlists.length > 0 && (
        <div className="scope-row">
          <label className="scope-label" htmlFor="playback-scope">
            {t("mode.scope")}
          </label>
          <select
            id="playback-scope"
            className="scope-select"
            value={activePlaylist ?? ""}
            onChange={(e) => onActivePlaylistChange(e.target.value || null)}
          >
            <option value="">{t("mode.allTracks")}</option>
            {playlists.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name} ({p.count})
              </option>
            ))}
          </select>
        </div>
      )}
    </div>
  );
}
