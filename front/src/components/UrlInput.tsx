import { useRef, useState } from "react";
import { searchYouTube } from "../api";
import { showApiError } from "../errors";
import { t } from "../i18n";
import { TrackRowInfo } from "./TrackRowInfo";
import type { Track } from "../types";

interface Props {
  /// Returns whether the request actually went out. A rejected one leaves the
  /// text where it is, so it can be sent again once the socket is back.
  onExtract: (url: string) => boolean;
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

export function UrlInput({ onExtract, onUnauthorized, showToast }: Props) {
  const [value, setValue] = useState("");
  const [searching, setSearching] = useState(false);
  const [results, setResults] = useState<Track[] | null>(null);
  const pastedRef = useRef(false);
  const searchRef = useRef<AbortController>(null);

  const isUrl = isYoutubeUrl(value);

  /// Drop any in-flight search so a late response cannot overwrite the results
  /// the user is now looking at.
  function cancelSearch() {
    searchRef.current?.abort();
    searchRef.current = null;
    setSearching(false);
  }

  function submit(input: string) {
    const trimmed = input.trim();
    if (!trimmed) {
      showToast(t("url.empty"));
      return;
    }
    if (isYoutubeUrl(trimmed)) {
      // Hand the box back the moment the request is out. The server puts the
      // request on display in the list below and keeps it there across a
      // reload, so holding the input until it finishes would do nothing but
      // keep the next URL from being queued behind it.
      if (!onExtract(trimmed)) return;
      cancelSearch();
      setValue("");
      setResults(null);
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
        <button className="btn" onClick={() => submit(value)}>
          {searching ? <><span className="spinner" />{t("url.searching")}</>
            : isUrl ? t("url.extract") : t("url.search")}
        </button>
      </div>

      {/* The query and the results stay up as tracks are picked: one search
          often yields several worth fetching, and each pick is independent. */}
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
            <div
              key={result.id}
              className="history-item"
              onClick={() => onExtract(`https://www.youtube.com/watch?v=${result.id}`)}
            >
              <TrackRowInfo track={result} />
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
