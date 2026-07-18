import type { PlaybackMode, Playlist } from "../types";

const MODES: { value: PlaybackMode; label: string; hint: string }[] = [
  { value: "off", label: "オフ", hint: "曲が終わったら停止" },
  { value: "loop", label: "ループ", hint: "再生範囲を順に連続再生" },
  { value: "shuffle", label: "シャッフル", hint: "再生範囲からランダムに連続再生" },
];

interface Props {
  mode: PlaybackMode;
  onChange: (mode: PlaybackMode) => void;
  playlists: Playlist[];
  /** ループ/シャッフルの選曲範囲プレイリスト ID (null はライブラリ全体) */
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
      {playlists.length > 0 && (
        <div className="scope-row">
          <label className="scope-label" htmlFor="playback-scope">
            再生範囲
          </label>
          <select
            id="playback-scope"
            className="scope-select"
            // 削除済みプレイリストへの追従はサーバー (active_playlist_update) に任せる
            value={activePlaylist ?? ""}
            onChange={(e) => onActivePlaylistChange(e.target.value || null)}
          >
            <option value="">ライブラリ全体</option>
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
