import { t } from "../i18n";
import type { Playlist } from "../types";

interface Props {
  playlists: Playlist[];
  onAdd: (playlistId: string) => void;
  onClose: () => void;
}

export function AddToPlaylistMenu({ playlists, onAdd, onClose }: Props) {
  return (
    <>
      <div className="menu-overlay" onClick={(e) => { e.stopPropagation(); onClose(); }} />
      <div className="playlist-menu" onClick={(e) => e.stopPropagation()}>
        <div className="playlist-menu-title">{t("playlistMenu.title")}</div>
        {playlists.length === 0 ? (
          <div className="playlist-menu-empty">
            {t("playlistMenu.empty")}
          </div>
        ) : (
          playlists.map((p) => (
            <button
              key={p.id}
              className="playlist-menu-item"
              onClick={() => onAdd(p.id)}
            >
              {p.name}
            </button>
          ))
        )}
      </div>
    </>
  );
}
