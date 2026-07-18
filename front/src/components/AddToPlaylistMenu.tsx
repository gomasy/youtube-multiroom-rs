import type { Playlist } from "../types";

interface Props {
  playlists: Playlist[];
  onAdd: (playlistId: string) => void;
  onClose: () => void;
}

/** トラック行の「プレイリストに追加」ドロップダウン */
export function AddToPlaylistMenu({ playlists, onAdd, onClose }: Props) {
  return (
    <>
      {/* メニュー外のクリックで閉じるための透明オーバーレイ */}
      <div className="menu-overlay" onClick={(e) => { e.stopPropagation(); onClose(); }} />
      <div className="playlist-menu" onClick={(e) => e.stopPropagation()}>
        <div className="playlist-menu-title">プレイリストに追加</div>
        {playlists.length === 0 ? (
          <div className="playlist-menu-empty">
            プレイリストがありません。一覧上部の「＋」で作成できます
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
