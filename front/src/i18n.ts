type Lang = "en" | "ja";

const lang: Lang = navigator.language.startsWith("ja") ? "ja" : "en";

// Reflect the detected locale on the document root for accessibility/CSS.
if (typeof document !== "undefined") {
  document.documentElement.lang = lang;
}

const messages: Record<Lang, Record<string, string>> = {
  en: {
    // Header
    "header.connected": "Connected to server",
    "header.disconnected": "Disconnected",

    // UrlInput
    "url.placeholder": "YouTube URL or search keywords...",
    "url.empty": "Please enter a URL or search keywords",
    "url.notYoutube": "Not a YouTube URL",
    "url.extracting": "Fetching",
    "url.searching": "Searching",
    "url.extract": "Fetch",
    "url.search": "Search",
    "url.results": "Search results",
    "url.close": "Close",
    "url.noResults": "No results found",

    // NowPlaying
    "nowPlaying.noTrack": "No track selected",
    "nowPlaying.hint": "Enter a YouTube URL to fetch a track",

    // DeviceList
    "status.idle": "Idle",
    "status.playing": "Playing",
    "status.paused": "Paused",
    "status.stopped": "Stopped",
    "status.queued": "Queued",
    "status.error": "Error",
    "devices.label": "Devices",
    "devices.empty": "No devices connected yet",
    "devices.emptyHint":
      'Say "Alexa, open YouTube Player" on an Echo\nto register a device',
    "devices.deleteFailed": "Failed to delete",
    "devices.deleted": "Device deleted",
    "devices.seekFailed": "Failed to seek",
    "devices.seekQueued":
      'Seek queued. Say "Alexa, open YouTube Player" to apply.',
    "devices.selectTrack": "Please fetch a track first",
    "devices.selectDevice": "Please select a device",
    "devices.playQueued": "Playback queued",
    "devices.queuedNext": "Added to Up Next",
    "devices.upNext": "Up Next",
    "devices.clearQueue": "Clear",
    "devices.removeFromQueue": "Remove from queue",
    "devices.deleteDevice": "Delete device",
    "devices.queueing": "Queueing",
    "devices.playSelected": "▶ Play on selected",
    "devices.adding": "Adding",
    "devices.addToUpNext": "Add to Up Next",
    "devices.selectAll": "Select all",

    // PlaybackModeSelector
    "mode.off": "Off",
    "mode.off.hint": "Stop after track ends",
    "mode.loop": "Loop",
    "mode.loop.hint": "Play scope tracks in order",
    "mode.shuffle": "Shuffle",
    "mode.shuffle.hint": "Play random tracks from scope",
    "mode.label": "Continuous play",
    "mode.scope": "Play scope",
    "mode.allTracks": "All tracks",

    // History
    "history.library": "Library",
    "history.createPlaylist": "Create playlist",
    "history.playlistName": "Playlist name",
    "history.create": "Create",
    "history.cancel": "Cancel",
    "history.tracks": "Tracks",
    "history.deletePlaylist": "Delete playlist",
    "history.playlistEmpty":
      "This playlist is empty. Use the add button in the library to add tracks.",
    "history.noTracks": "No tracks",
    "history.dragToReorder": "Drag to reorder",
    "history.addToPlaylist": "Add to playlist",
    "history.removeFromPlaylist": "Remove from playlist",
    "history.deleteTrack": "Delete track",
    "history.deleteFailed": "Failed to delete",
    "history.trackDeleted": "Track deleted",
    "history.removedFromPlaylist": "Removed from playlist",
    "history.playlistCreated": "Playlist created",
    "history.playlistDeleted": "Playlist deleted",
    "history.addedToPlaylist": "Added to playlist",
    "history.prev": "Prev",
    "history.next": "Next",

    // AuthModal
    "auth.required": "Authentication required",
    "auth.enterToken": "Enter your API token",
    "auth.invalidToken": "Invalid token",
    "auth.connect": "Connect",

    // DownloadList
    "download.metadata": "Fetching info...",
    "download.processing": "Converting...",
    "download.error": "Error",

    // PreviewPlayer
    "preview.playbackFailed": "Preview playback failed",
    "preview.pause": "Pause preview",
    "preview.play": "Preview in browser",
    "preview.position": "Preview position",

    // SeekBar
    "seek.position": "Playback position",

    // AddToPlaylistMenu
    "playlistMenu.title": "Add to playlist",
    "playlistMenu.empty": 'No playlists. Create one with the "+" button above.',

    // App / common
    "common.trackFetched": "Fetched",
    "common.error": "Error",
    "common.importStarted": "Started importing {total} tracks from playlist",
    "common.notConnected": "Not connected to server",

    // api.ts
    "api.unauthorized": "Authentication required",
    "api.fetchTracksFailed": "Failed to fetch track list",
    "api.reorderFailed": "Failed to reorder",
    "api.streamUrlFailed": "Failed to get playback URL",
    "api.createPlaylistFailed": "Failed to create playlist",
    "api.deletePlaylistFailed": "Failed to delete playlist",
    "api.addToPlaylistFailed": "Failed to add to playlist",
    "api.removeFromPlaylistFailed": "Failed to remove from playlist",
    "api.searchFailed": "Search failed",
    "api.playFailed": "Failed to queue playback",
    "api.queueFailed": "Failed to add to queue",
    "api.removeQueueFailed": "Failed to remove from queue",
    "api.clearQueueFailed": "Failed to clear queue",
  },
  ja: {
    // Header
    "header.connected": "サーバー接続中",
    "header.disconnected": "切断中",

    // UrlInput
    "url.placeholder": "YouTube URL または検索キーワード...",
    "url.empty": "URL または検索キーワードを入力してください",
    "url.notYoutube": "YouTube の URL ではないため取得できません",
    "url.extracting": "取得中",
    "url.searching": "検索中",
    "url.extract": "取得",
    "url.search": "検索",
    "url.results": "検索結果",
    "url.close": "閉じる",
    "url.noResults": "見つかりませんでした",

    // NowPlaying
    "nowPlaying.noTrack": "曲が選択されていません",
    "nowPlaying.hint": "YouTube URL を入力して取得してください",

    // DeviceList
    "status.idle": "待機中",
    "status.playing": "再生中",
    "status.paused": "一時停止",
    "status.stopped": "停止",
    "status.queued": "キュー済み",
    "status.error": "エラー",
    "devices.label": "デバイス",
    "devices.empty": "まだデバイスが接続されていません",
    "devices.emptyHint":
      "Echo で「アレクサ、YouTube プレーヤーを開いて」と\n言うとデバイスが登録されます",
    "devices.deleteFailed": "削除に失敗しました",
    "devices.deleted": "デバイスを削除しました",
    "devices.seekFailed": "シークに失敗しました",
    "devices.seekQueued":
      "シークをキューしました。「アレクサ、YouTube プレーヤーを開いて」で反映されます。",
    "devices.selectTrack": "先にトラックを取得してください",
    "devices.selectDevice": "デバイスを選択してください",
    "devices.playQueued": "再生をキューしました",
    "devices.queuedNext": "次に再生に追加しました",
    "devices.upNext": "次に再生",
    "devices.clearQueue": "クリア",
    "devices.removeFromQueue": "キューから削除",
    "devices.deleteDevice": "デバイスを削除",
    "devices.queueing": "キュー中",
    "devices.playSelected": "▶ 選択デバイスで再生",
    "devices.adding": "追加中",
    "devices.addToUpNext": "次に再生に追加",
    "devices.selectAll": "全選択",

    // PlaybackModeSelector
    "mode.off": "オフ",
    "mode.off.hint": "曲が終わったら停止",
    "mode.loop": "ループ",
    "mode.loop.hint": "再生範囲を順に連続再生",
    "mode.shuffle": "シャッフル",
    "mode.shuffle.hint": "再生範囲からランダムに連続再生",
    "mode.label": "連続再生",
    "mode.scope": "再生範囲",
    "mode.allTracks": "ライブラリ全体",

    // History
    "history.library": "ライブラリ",
    "history.createPlaylist": "プレイリストを作成",
    "history.playlistName": "プレイリスト名",
    "history.create": "作成",
    "history.cancel": "キャンセル",
    "history.tracks": "取得済みトラック",
    "history.deletePlaylist": "プレイリストを削除",
    "history.playlistEmpty":
      "このプレイリストは空です。ライブラリの ♪＋ ボタンで追加できます。",
    "history.noTracks": "トラックがありません",
    "history.dragToReorder": "ドラッグで並べ替え",
    "history.addToPlaylist": "プレイリストに追加",
    "history.removeFromPlaylist": "プレイリストから外す",
    "history.deleteTrack": "トラックを削除",
    "history.deleteFailed": "削除に失敗しました",
    "history.trackDeleted": "トラックを削除しました",
    "history.removedFromPlaylist": "プレイリストから外しました",
    "history.playlistCreated": "プレイリストを作成しました",
    "history.playlistDeleted": "プレイリストを削除しました",
    "history.addedToPlaylist": "プレイリストに追加しました",
    "history.prev": "前へ",
    "history.next": "次へ",

    // AuthModal
    "auth.required": "認証が必要です",
    "auth.enterToken": "API トークンを入力してください",
    "auth.invalidToken": "トークンが正しくありません",
    "auth.connect": "接続",

    // DownloadList
    "download.metadata": "情報取得中...",
    "download.processing": "変換中...",
    "download.error": "エラー",

    // PreviewPlayer
    "preview.playbackFailed": "試聴の再生に失敗しました",
    "preview.pause": "試聴を一時停止",
    "preview.play": "ブラウザで試聴",
    "preview.position": "試聴位置",

    // SeekBar
    "seek.position": "再生位置",

    // AddToPlaylistMenu
    "playlistMenu.title": "プレイリストに追加",
    "playlistMenu.empty":
      "プレイリストがありません。一覧上部の「＋」で作成できます。",

    // App / common
    "common.trackFetched": "取得しました",
    "common.error": "エラー",
    "common.importStarted":
      "プレイリストから {total} 曲の取り込みを開始しました",
    "common.notConnected": "サーバーに接続されていません",

    // api.ts
    "api.unauthorized": "認証が必要です",
    "api.fetchTracksFailed": "トラック一覧の取得に失敗しました",
    "api.reorderFailed": "並べ替えに失敗しました",
    "api.streamUrlFailed": "再生 URL の取得に失敗しました",
    "api.createPlaylistFailed": "プレイリストの作成に失敗しました",
    "api.deletePlaylistFailed": "プレイリストの削除に失敗しました",
    "api.addToPlaylistFailed": "プレイリストへの追加に失敗しました",
    "api.removeFromPlaylistFailed": "プレイリストからの削除に失敗しました",
    "api.searchFailed": "検索に失敗しました",
    "api.playFailed": "再生のキューに失敗しました",
    "api.queueFailed": "キューへの追加に失敗しました",
    "api.removeQueueFailed": "キューからの削除に失敗しました",
    "api.clearQueueFailed": "キューのクリアに失敗しました",
  },
};

export function t(key: string): string {
  return messages[lang][key] ?? messages["en"][key] ?? key;
}

export function tFmt(key: string, params: Record<string, string | number>): string {
  let msg = t(key);
  for (const [k, v] of Object.entries(params)) {
    // Replace every occurrence (String.replace with a string only swaps the first).
    msg = msg.replace(new RegExp(`\\{${k}\\}`, "g"), String(v));
  }
  return msg;
}

export function getLang(): Lang {
  return lang;
}
