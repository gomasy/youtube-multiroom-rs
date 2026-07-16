import { useRef, useState, useImperativeHandle, forwardRef } from "react";
import { searchYouTube } from "../api";
import { TrackRowInfo } from "./TrackRowInfo";
import type { Track } from "../types";

export interface UrlInputHandle {
  clear: () => void;
}

interface Props {
  extracting: boolean;
  onExtract: (url: string) => void;
  onUnauthorized: () => void;
  showToast: (msg: string) => void;
}

function isYoutubeUrl(value: string): boolean {
  return /youtube\.com|youtu\.be/.test(value);
}

export const UrlInput = forwardRef<UrlInputHandle, Props>(function UrlInput(
  { extracting, onExtract, onUnauthorized, showToast },
  ref,
) {
  const [value, setValue] = useState("");
  const [searching, setSearching] = useState(false);
  const [results, setResults] = useState<Track[] | null>(null);
  // 直前の onChange がペースト由来かどうか (URL ペースト時の自動取得に使う)
  const pastedRef = useRef(false);

  const busy = extracting || searching;
  const isUrl = isYoutubeUrl(value);

  useImperativeHandle(ref, () => ({
    clear: () => setValue(""),
  }));

  function submit(input: string) {
    if (busy) return;
    const trimmed = input.trim();
    if (!trimmed) {
      showToast("URL または検索キーワードを入力してください");
      return;
    }
    if (isYoutubeUrl(trimmed)) {
      setResults(null);
      onExtract(trimmed);
      return;
    }
    // YouTube 以外の URL は検索キーワード扱いせず、非対応であることを伝える
    if (/^https?:\/\//i.test(trimmed)) {
      showToast("YouTube の URL ではないため取得できません");
      return;
    }
    void search(trimmed);
  }

  async function search(query: string) {
    setSearching(true);
    try {
      setResults(await searchYouTube(query, onUnauthorized));
    } catch (e) {
      showToast(`エラー: ${(e as Error).message}`);
    } finally {
      setSearching(false);
    }
  }

  function pickResult(track: Track) {
    if (extracting) return;
    setResults(null);
    onExtract(`https://www.youtube.com/watch?v=${track.id}`);
  }

  function handleChange(e: React.ChangeEvent<HTMLInputElement>) {
    const next = e.target.value;
    setValue(next);
    if (pastedRef.current) {
      pastedRef.current = false;
      if (isYoutubeUrl(next.trim())) submit(next);
    }
  }

  return (
    <div className="url-section">
      <div className="url-row">
        <input
          type="text"
          className="url-input"
          placeholder="YouTube URL または検索キーワード..."
          autoComplete="off"
          spellCheck={false}
          value={value}
          onChange={handleChange}
          onKeyDown={(e) => { if (e.key === "Enter") submit(value); }}
          onPaste={() => { pastedRef.current = true; }}
        />
        <button className="btn" onClick={() => submit(value)} disabled={busy}>
          {extracting ? <><span className="spinner" />取得中</>
            : searching ? <><span className="spinner" />検索中</>
            : isUrl ? "取得" : "検索"}
        </button>
      </div>

      {results && (
        <div className="search-results">
          <div className="search-results-header section-label">
            <span>検索結果 ({results.length})</span>
            <button className="text-btn" onClick={() => setResults(null)}>
              閉じる
            </button>
          </div>
          {results.length === 0 && (
            <div className="search-empty">見つかりませんでした</div>
          )}
          {results.map((t) => (
            <div key={t.id} className="history-item" onClick={() => pickResult(t)}>
              <TrackRowInfo track={t} />
            </div>
          ))}
        </div>
      )}
    </div>
  );
});
