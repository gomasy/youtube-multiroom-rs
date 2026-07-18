import { useState } from "react";
import { authFetch, playTracks, queueNext, removeQueueItem, clearQueue } from "../api";
import { ScrollingText } from "./ScrollingText";
import { SeekBar } from "./SeekBar";
import { CloseIcon, TrashIcon } from "./icons";
import type { Device, Track } from "../types";

const STATUS_LABELS: Record<string, string> = {
  idle: "待機中",
  playing: "再生中",
  paused: "一時停止",
  stopped: "停止",
  queued: "キュー済み",
  error: "エラー",
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
      const res = await authFetch(
        `/api/devices/${encodeURIComponent(deviceId)}`,
        { method: "DELETE" },
        onUnauthorized,
      );
      if (!res.ok) throw new Error("削除に失敗しました");
      setSelectedDevices((prev) => {
        const next = new Set(prev);
        next.delete(deviceId);
        return next;
      });
      onDeviceDeleted(deviceId);
      showToast("デバイスを削除しました");
    } catch (e) {
      showToast(`エラー: ${(e as Error).message}`);
    }
  }

  async function seekDevice(deviceId: string, positionMs: number) {
    try {
      const res = await authFetch(
        `/api/devices/${encodeURIComponent(deviceId)}/seek`,
        {
          method: "POST",
          body: JSON.stringify({ position_ms: positionMs }),
        },
        onUnauthorized,
      );
      if (!res.ok) throw new Error("シークに失敗しました");
      showToast("シークをキューしました。「アレクサ、YouTube プレーヤーを開いて」で反映されます");
    } catch (e) {
      showToast(`エラー: ${(e as Error).message}`);
    }
  }

  // 選択デバイスへの操作 (再生 / 次に再生) の検証・busy 管理・結果トーストを共通化する
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
      showToast("先にトラックを取得してください");
      return;
    }
    if (selectedDevices.size === 0) {
      showToast("デバイスを選択してください");
      return;
    }

    setBusy(true);
    try {
      const data = await call(currentTrack.id, Array.from(selectedDevices), onUnauthorized);
      showToast(data.message || fallbackMsg);
    } catch (e) {
      showToast(`エラー: ${(e as Error).message}`);
    } finally {
      setBusy(false);
    }
  }

  const playOnSelected = () => sendToSelected(playTracks, setPlaying, "再生をキューしました");
  const queueOnSelected = () => sendToSelected(queueNext, setQueueing, "次に再生に追加しました");

  // 成功時の表示更新はサーバーが配る device_update に任せ、失敗だけ通知する
  function catchToast(action: Promise<unknown>) {
    action.catch((e) => showToast(`エラー: ${(e as Error).message}`));
  }

  const canPlay = !!currentTrack && selectedDevices.size > 0;

  return (
    <div className="devices-section">
      <div className="section-label">デバイス</div>
      <div className="device-list">
        {entries.length === 0 ? (
          <div className="empty-state">
            <svg width="36" height="36" viewBox="0 0 24 24" fill="none">
              <circle cx="12" cy="12" r="10" stroke="var(--text-dim)" strokeWidth="1.5" />
              <path d="M12 8v4M12 14.5v.5" stroke="var(--text-dim)" strokeWidth="1.5" strokeLinecap="round" />
            </svg>
            <div>まだデバイスが接続されていません</div>
            <div style={{ marginTop: 6, fontSize: 12 }}>
              Echo で「アレクサ、YouTube プレーヤーを開いて」と<br />
              言うとデバイスが登録されます
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
                    {STATUS_LABELS[dev.status] || dev.status}
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
                        <span>次に再生 ({dev.queue.length})</span>
                        <button
                          className="text-btn text-btn-danger"
                          onClick={() => catchToast(clearQueue(dev.device_id, onUnauthorized))}
                        >
                          クリア
                        </button>
                      </div>
                      {dev.queue.map((t, i) => (
                        <div key={t.entry} className="device-queue-item">
                          <span className="queue-item-index">{i + 1}</span>
                          <span className="queue-item-title">{t.title}</span>
                          <button
                            className="delete-btn"
                            title="キューから削除"
                            onClick={() =>
                              catchToast(removeQueueItem(dev.device_id, t.entry, onUnauthorized))
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
                  title="デバイスを削除"
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
            {playing ? <><span className="spinner" />キュー中</> : "▶ 選択デバイスで再生"}
          </button>
          <button
            className="btn btn-outline"
            onClick={queueOnSelected}
            disabled={!canPlay || queueing}
          >
            {queueing ? <><span className="spinner" />追加中</> : "次に再生に追加"}
          </button>
          <button className="btn btn-outline btn-sm" onClick={selectAll}>
            全選択
          </button>
        </div>
      )}
    </div>
  );
}
