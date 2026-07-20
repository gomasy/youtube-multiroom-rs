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
  /** Last known playback position (milliseconds) */
  position_ms?: number;
  /** Last device state update time (UNIX seconds) */
  last_update?: number;
  /** Up Next queue (front item plays next) */
  queue?: QueueItem[];
}

/** A single item in the Up Next queue */
export interface QueueItem extends Track {
  /** Unique entry identifier within the queue. Used for deletion. */
  entry: string;
}

export interface TracksPage {
  tracks: Track[];
  total: number;
  page: number;
  per_page: number;
}

/** Named playlist (wire format with track count) */
export interface Playlist {
  id: string;
  name: string;
  /** Creation time (UNIX seconds). List is sorted ascending by this. */
  created_at?: number;
  count: number;
}

export type PlaybackMode = "loop" | "shuffle" | "off";

/** In-progress download progress (managed server-side, broadcast to all clients) */
export interface DownloadProgress {
  id: string;
  /** Contains video ID before metadata is fetched */
  title: string;
  status: "metadata" | "downloading" | "processing" | "error";
  /** Downloaded percentage (0–100) */
  percent: number;
  error?: string;
  /** Start time (UNIX seconds) */
  started_at: number;
}

export interface WSInitMessage {
  type: "init";
  version?: string;
  devices: Record<string, Device>;
  playback_mode?: PlaybackMode;
  downloads?: DownloadProgress[];
  playlists?: Playlist[];
  /** Loop/shuffle scope playlist ID (null means full library) */
  active_playlist?: string | null;
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

export interface WSPlaylistsUpdateMessage {
  type: "playlists_update";
  playlists: Playlist[];
}

export interface WSActivePlaylistUpdateMessage {
  type: "active_playlist_update";
  /** null means full library */
  playlist: string | null;
}

/** Response indicating a playlist URL batch import has started (import runs in background) */
export interface WSPlaylistImportResultMessage {
  type: "playlist_import_result";
  name: string;
  total: number;
}

export type WSMessage =
  | WSInitMessage
  | WSDeviceUpdateMessage
  | WSTracksUpdateMessage
  | WSPlaybackModeUpdateMessage
  | WSExtractAudioResultMessage
  | WSExtractAudioErrorMessage
  | WSDownloadsUpdateMessage
  | WSPlaylistsUpdateMessage
  | WSActivePlaylistUpdateMessage
  | WSPlaylistImportResultMessage;
