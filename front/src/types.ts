export interface Track {
  id: string;
  title: string;
  thumbnail?: string;
  duration?: number;
  channel?: string;
}

export interface Device {
  device_id: string;
  name: string;
  status: "idle" | "playing" | "paused" | "stopped" | "queued" | "error";
  current_track?: Track;
}

export interface WSInitMessage {
  type: "init";
  devices: Record<string, Device>;
  tracks: Record<string, Track>;
}

export interface WSDeviceUpdateMessage {
  type: "device_update";
  devices: Record<string, Device>;
}

export interface WSTracksUpdateMessage {
  type: "tracks_update";
  tracks: Record<string, Track>;
}

export type WSMessage = WSInitMessage | WSDeviceUpdateMessage | WSTracksUpdateMessage;
