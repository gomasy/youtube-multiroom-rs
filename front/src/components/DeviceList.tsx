import { useState } from "react";
import { authOk, playTracks, queueNext, removeQueueItem, clearQueue } from "../api";
import { t } from "../i18n";
import { ScrollingText } from "./ScrollingText";
import { SeekBar } from "./SeekBar";
import { CloseIcon, TrashIcon } from "./icons";
import type { Device, Track } from "../types";

const STATUS_KEYS: Record<string, string> = {
  idle: "status.idle",
  playing: "status.playing",
  paused: "status.paused",
  stopped: "status.stopped",
  queued: "status.queued",
  error: "status.error",
};

interface Props {
  devices: Record<string, Device>;
  currentTrack: Track | null;
  onDeviceDeleted: (deviceId: string) => void;
  onUnauthorized: () => void;
  showToast: (msg: string) => void;
}

export function DeviceList({ devices, currentTrack, onDeviceDeleted, onUnauthorized, showToast }: Props) {
  const [selectedDevices, setSelectedDevices] = useState<Set<string>>(new Set());
  const [playing, setPlaying] = useState(false);
  const [queueing, setQueueing] = useState(false);
  const entries = Object.values(devices);

  function toggleDevice(deviceId: string) {
    setSelectedDevices((prev) => {
      const next = new Set(prev);
      if (next.has(deviceId)) next.delete(deviceId);
      else next.add(deviceId);
      return next;
    });
  }

  function selectAll() {
    const allIds = Object.keys(devices);
    setSelectedDevices((prev) =>
      prev.size === allIds.length ? new Set() : new Set(allIds),
    );
  }

  async function deleteDevice(deviceId: string) {
    try {
      await authOk(
        `/api/devices/${encodeURIComponent(deviceId)}`,
        "devices.deleteFailed",
        { method: "DELETE" },
        onUnauthorized,
      );
      setSelectedDevices((prev) => {
        const next = new Set(prev);
        next.delete(deviceId);
        return next;
      });
      onDeviceDeleted(deviceId);
      showToast(t("devices.deleted"));
    } catch (e) {
      showToast(`${t("common.error")}: ${(e as Error).message}`);
    }
  }

  async function seekDevice(deviceId: string, positionMs: number) {
    try {
      await authOk(
        `/api/devices/${encodeURIComponent(deviceId)}/seek`,
        "devices.seekFailed",
        {
          method: "POST",
          body: JSON.stringify({ position_ms: positionMs }),
        },
        onUnauthorized,
      );
      showToast(t("devices.seekQueued"));
    } catch (e) {
      showToast(`${t("common.error")}: ${(e as Error).message}`);
    }
  }

  async function sendToSelected(
    call: (
      trackId: string,
      deviceIds: string[],
      onUnauthorized?: () => void,
    ) => Promise<{ message?: string }>,
    setBusy: (busy: boolean) => void,
    fallbackMsg: string,
  ) {
    if (!currentTrack) {
      showToast(t("devices.selectTrack"));
      return;
    }
    if (selectedDevices.size === 0) {
      showToast(t("devices.selectDevice"));
      return;
    }

    setBusy(true);
    try {
      const data = await call(currentTrack.id, Array.from(selectedDevices), onUnauthorized);
      showToast(data.message || fallbackMsg);
    } catch (e) {
      showToast(`${t("common.error")}: ${(e as Error).message}`);
    } finally {
      setBusy(false);
    }
  }

  const playOnSelected = () => sendToSelected(playTracks, setPlaying, t("devices.playQueued"));
  const queueOnSelected = () => sendToSelected(queueNext, setQueueing, t("devices.queuedNext"));

  function catchToast(action: Promise<unknown>) {
    action.catch((e) => showToast(`${t("common.error")}: ${(e as Error).message}`));
  }

  const canPlay = !!currentTrack && selectedDevices.size > 0;

  return (
    <div className="devices-section">
      <div className="section-label">{t("devices.label")}</div>
      <div className="device-list">
        {entries.length === 0 ? (
          <div className="empty-state">
            <svg width="36" height="36" viewBox="0 0 24 24" fill="none">
              <circle cx="12" cy="12" r="10" stroke="var(--text-dim)" strokeWidth="1.5" />
              <path d="M12 8v4M12 14.5v.5" stroke="var(--text-dim)" strokeWidth="1.5" strokeLinecap="round" />
            </svg>
            <div>{t("devices.empty")}</div>
            <div style={{ marginTop: 6, fontSize: 12, whiteSpace: "pre-line" }}>
              {t("devices.emptyHint")}
            </div>
          </div>
        ) : (
          entries.map((dev) => {
            const selected = selectedDevices.has(dev.device_id);
            return (
              <div
                key={dev.device_id}
                className={`device-card${selected ? " selected" : ""}`}
                onClick={() => toggleDevice(dev.device_id)}
              >
                <div className="device-check">
                  <div className="device-check-inner" />
                </div>
                <div className="device-info">
                  <div className="device-name">{dev.name}</div>
                  <div className="device-status">
                    <span className={`status-dot ${dev.status}`} />
                    {STATUS_KEYS[dev.status] ? t(STATUS_KEYS[dev.status]) : dev.status}
                  </div>
                  {dev.current_track && (
                    <ScrollingText
                      className="device-track-name"
                      text={`♪ ${dev.current_track.title}`}
                    />
                  )}
                  <SeekBar
                    device={dev}
                    onSeek={(pos) => seekDevice(dev.device_id, pos)}
                  />
                  {dev.queue && dev.queue.length > 0 && (
                    <div className="device-queue" onClick={(e) => e.stopPropagation()}>
                      <div className="device-queue-header">
                        <span>{t("devices.upNext")} ({dev.queue.length})</span>
                        <button
                          className="text-btn text-btn-danger"
                          onClick={() => catchToast(clearQueue(dev.device_id, onUnauthorized))}
                        >
                          {t("devices.clearQueue")}
                        </button>
                      </div>
                      {dev.queue.map((qt, i) => (
                        <div key={qt.entry} className="device-queue-item">
                          <span className="queue-item-index">{i + 1}</span>
                          <span className="queue-item-title">{qt.title}</span>
                          <button
                            className="delete-btn"
                            title={t("devices.removeFromQueue")}
                            onClick={() =>
                              catchToast(removeQueueItem(dev.device_id, qt.entry, onUnauthorized))
                            }
                          >
                            <CloseIcon size={12} />
                          </button>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
                <button
                  className="delete-btn"
                  title={t("devices.deleteDevice")}
                  onClick={(e) => { e.stopPropagation(); deleteDevice(dev.device_id); }}
                >
                  <TrashIcon />
                </button>
              </div>
            );
          })
        )}
      </div>

      {entries.length > 0 && (
        <div className="controls">
          <button className="btn" onClick={playOnSelected} disabled={!canPlay || playing}>
            {playing ? <><span className="spinner" />{t("devices.queueing")}</> : t("devices.playSelected")}
          </button>
          <button
            className="btn btn-outline"
            onClick={queueOnSelected}
            disabled={!canPlay || queueing}
          >
            {queueing ? <><span className="spinner" />{t("devices.adding")}</> : t("devices.addToUpNext")}
          </button>
          <button className="btn btn-outline btn-sm" onClick={selectAll}>
            {t("devices.selectAll")}
          </button>
        </div>
      )}
    </div>
  );
}
