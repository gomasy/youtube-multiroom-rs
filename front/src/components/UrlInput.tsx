import { useRef, useState, useImperativeHandle, forwardRef } from "react";
import { searchYouTube } from "../api";
import { t } from "../i18n";
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
      showToast(t("url.empty"));
      return;
    }
    if (isYoutubeUrl(trimmed)) {
      setResults(null);
      onExtract(trimmed);
      return;
    }
    if (/^https?:\/\//i.test(trimmed)) {
      showToast(t("url.notYoutube"));
      return;
    }
    void search(trimmed);
  }

  async function search(query: string) {
    setSearching(true);
    try {
      setResults(await searchYouTube(query, onUnauthorized));
    } catch (e) {
      showToast(`${t("common.error")}: ${(e as Error).message}`);
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
          placeholder={t("url.placeholder")}
          autoComplete="off"
          spellCheck={false}
          value={value}
          onChange={handleChange}
          onKeyDown={(e) => { if (e.key === "Enter") submit(value); }}
          onPaste={() => { pastedRef.current = true; }}
        />
        <button className="btn" onClick={() => submit(value)} disabled={busy}>
          {extracting ? <><span className="spinner" />{t("url.extracting")}</>
            : searching ? <><span className="spinner" />{t("url.searching")}</>
            : isUrl ? t("url.extract") : t("url.search")}
        </button>
      </div>

      {results && (
        <div className="search-results">
          <div className="search-results-header section-label">
            <span>{t("url.results")} ({results.length})</span>
            <button className="text-btn" onClick={() => setResults(null)}>
              {t("url.close")}
            </button>
          </div>
          {results.length === 0 && (
            <div className="search-empty">{t("url.noResults")}</div>
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
