import { useState, useRef } from "react";
import { authFetch } from "../api";
import type { Track } from "../types";

interface Props {
  onTrackExtracted: (track: Track) => void;
  onUnauthorized: () => void;
  showToast: (msg: string) => void;
}

export function UrlInput({ onTrackExtracted, onUnauthorized, showToast }: Props) {
  const [loading, setLoading] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  async function extract() {
    const url = inputRef.current?.value.trim();
    if (!url) {
      showToast("URLを入力してください");
      return;
    }

    setLoading(true);
    try {
      const res = await authFetch(
        "/api/audio/extract",
        { method: "POST", body: JSON.stringify({ url }) },
        onUnauthorized,
      );
      if (!res.ok) {
        const err = await res.json();
        throw new Error(err.detail || "取得に失敗しました");
      }
      const track: Track = await res.json();
      onTrackExtracted(track);
      showToast(`「${track.title}」を取得しました`);
      if (inputRef.current) inputRef.current.value = "";
    } catch (e) {
      showToast(`エラー: ${(e as Error).message}`);
    } finally {
      setLoading(false);
    }
  }

  function handleKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Enter") extract();
  }

  function handlePaste() {
    setTimeout(() => {
      const val = inputRef.current?.value.trim() || "";
      if (/youtube\.com|youtu\.be/.test(val)) {
        extract();
      }
    }, 100);
  }

  return (
    <div className="url-section">
      <div className="url-row">
        <input
          ref={inputRef}
          type="text"
          className="url-input"
          placeholder="YouTube URL を貼り付け..."
          autoComplete="off"
          spellCheck={false}
          onKeyDown={handleKeyDown}
          onPaste={handlePaste}
        />
        <button className="btn" onClick={extract} disabled={loading}>
          {loading ? <><span className="spinner" />取得中</> : "取得"}
        </button>
      </div>
    </div>
  );
}
