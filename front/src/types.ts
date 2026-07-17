export interface Track {
  id: string;
  title: string;
  thumbnail?: string;
  duration?: number;
  channel?: string;
  is_live?: boolean;
}

export interface Device {
  device_id: string;
  name: string;
  status: "idle" | "playing" | "paused" | "stopped" | "queued" | "error";
  current_track?: Track;
  /** 最後に確認できた再生位置 (ミリ秒) */
  position_ms?: number;
  /** デバイス状態の最終更新時刻 (UNIX 秒) */
  last_update?: number;
  /** 「次に再生」キュー (先頭が次に再生される) */
  queue?: QueueItem[];
}

/** 「次に再生」キューの 1 項目 */
export interface QueueItem extends Track {
  /** キュー内で一意なエントリ。削除時の指定に使う */
  entry: string;
}

export interface TracksPage {
  tracks: Track[];
  total: number;
  page: number;
  per_page: number;
}

export type PlaybackMode = "loop" | "shuffle" | "off";

/** 進行中ダウンロードの進捗 (サーバー側で管理され、全クライアントへ配られる) */
export interface DownloadProgress {
  id: string;
  /** メタデータ取得前は動画 ID が入る */
  title: string;
  status: "metadata" | "downloading" | "processing" | "error";
  /** ダウンロード済み割合 (0–100) */
  percent: number;
  error?: string;
  /** 開始時刻 (UNIX 秒) */
  started_at: number;
}

export interface WSInitMessage {
  type: "init";
  version?: string;
  devices: Record<string, Device>;
  playback_mode?: PlaybackMode;
  downloads?: DownloadProgress[];
}

export interface WSDeviceUpdateMessage {
  type: "device_update";
  devices: Record<string, Device>;
}

export interface WSTracksUpdateMessage {
  type: "tracks_update";
}

export interface WSPlaybackModeUpdateMessage {
  type: "playback_mode_update";
  mode: PlaybackMode;
}

export interface WSExtractAudioResultMessage {
  type: "extract_audio_result";
  track: Track;
}

export interface WSExtractAudioErrorMessage {
  type: "extract_audio_error";
  error: string;
}

export interface WSDownloadsUpdateMessage {
  type: "downloads_update";
  downloads: DownloadProgress[];
}

export type WSMessage =
  | WSInitMessage
  | WSDeviceUpdateMessage
  | WSTracksUpdateMessage
  | WSPlaybackModeUpdateMessage
  | WSExtractAudioResultMessage
  | WSExtractAudioErrorMessage
  | WSDownloadsUpdateMessage;
