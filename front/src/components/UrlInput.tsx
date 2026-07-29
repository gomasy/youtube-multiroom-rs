import { useRef, useState, useImperativeHandle, forwardRef } from "react";
import { searchYouTube } from "../api";
import { showApiError } from "../errors";
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

const SCHEME_RE = /^[a-z][a-z\d+.-]*:\/\//i;
/// Only decides the button label and paste auto-submit. The authoritative check
/// is parse_youtube_url() in src/state/url.rs — keep the host list in sync with it.
const YOUTUBE_HOSTS = new Set([
  "youtube.com",
  "www.youtube.com",
  "m.youtube.com",
  "music.youtube.com",
  "youtu.be",
]);

function isYoutubeUrl(value: string): boolean {
  try {
    const url = new URL(SCHEME_RE.test(value) ? value : `https://${value}`);
    return (
      (url.protocol === "https:" || url.protocol === "http:") &&
      YOUTUBE_HOSTS.has(url.hostname.toLowerCase())
    );
  } catch {
    return false;
  }
}

export const UrlInput = forwardRef<UrlInputHandle, Props>(function UrlInput(
  { extracting, onExtract, onUnauthorized, showToast },
  ref,
) {
  const [value, setValue] = useState("");
  const [searching, setSearching] = useState(false);
  const [results, setResults] = useState<Track[] | null>(null);
  const pastedRef = useRef(false);
  const searchRef = useRef<AbortController>(null);

  const busy = extracting || searching;
  const isUrl = isYoutubeUrl(value);

  /// Drop any in-flight search so a late response cannot overwrite the results
  /// the user is now looking at.
  function cancelSearch() {
    searchRef.current?.abort();
    searchRef.current = null;
    setSearching(false);
  }

  useImperativeHandle(ref, () => ({
    clear: () => {
      cancelSearch();
      setValue("");
      setResults(null);
    },
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
    cancelSearch();
    const request = new AbortController();
    searchRef.current = request;
    setSearching(true);
    try {
      const nextResults = await searchYouTube(query, onUnauthorized, request.signal);
      if (request.signal.aborted) return;
      setResults(nextResults);
    } catch (e) {
      showApiError(showToast, e);
    } finally {
      if (!request.signal.aborted) setSearching(false);
    }
  }

  function pickResult(track: Track) {
    if (extracting) return;
    setResults(null);
    onExtract(`https://www.youtube.com/watch?v=${track.id}`);
  }

  function handleChange(e: React.ChangeEvent<HTMLInputElement>) {
    const next = e.target.value;
    cancelSearch();
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
          {results.map((result) => (
            <div key={result.id} className="history-item" onClick={() => pickResult(result)}>
              <TrackRowInfo track={result} />
            </div>
          ))}
        </div>
      )}
    </div>
  );
});
