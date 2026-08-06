import { useCallback, useState } from "react";
import {
  cleanupCache,
  exportLibrary,
  fetchCacheStatus,
  importLibrary,
  repairCache,
} from "../api";
import { showApiError } from "../errors";
import { formatBytes } from "../format";
import { t, tFmt } from "../i18n";
import type { CacheStatus } from "../types";

/** Hand a string to the browser as a file download. */
function saveAs(text: string, name: string) {
  const url = URL.createObjectURL(new Blob([text], { type: "application/json" }));
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  link.click();
  URL.revokeObjectURL(url);
}

interface Props {
  onUnauthorized: () => void;
  showToast: (msg: string) => void;
}

/**
 * Cache upkeep and library backup: the two things that need doing occasionally
 * and never while listening.
 *
 * Collapsed until asked for, which is also what keeps the cache scan off the
 * page load — it walks the whole cache directory, and nothing here changes
 * often enough to be worth watching.
 */
export function LibraryTools({ onUnauthorized, showToast }: Props) {
  const [open, setOpen] = useState(false);
  const [status, setStatus] = useState<CacheStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [withAudio, setWithAudio] = useState(false);

  const load = useCallback(async () => {
    try {
      setStatus(await fetchCacheStatus(onUnauthorized));
    } catch (e) {
      showApiError(showToast, e);
    }
  }, [onUnauthorized, showToast]);

  function toggle() {
    const next = !open;
    setOpen(next);
    if (next) void load();
  }

  /** Disable the panel for the duration of `action`, reporting what it throws. */
  async function withBusy(action: () => Promise<void>) {
    setBusy(true);
    try {
      await action();
    } catch (e) {
      showApiError(showToast, e);
    } finally {
      setBusy(false);
    }
  }

  /** Run an action that reports a message and changes what the panel shows. */
  function run(action: () => Promise<{ message?: string }>, fallback: string) {
    return withBusy(async () => {
      const data = await action();
      showToast(data.message || fallback);
      await load();
    });
  }

  function save() {
    return withBusy(async () => {
      const doc = await exportLibrary(onUnauthorized);
      saveAs(doc, `youtube-multiroom-${new Date().toISOString().slice(0, 10)}.json`);
      showToast(t("tools.exported"));
    });
  }

  async function restore(file: File) {
    let doc: Record<string, unknown>;
    try {
      doc = JSON.parse(await file.text()) as Record<string, unknown>;
    } catch {
      // Reported on its own terms: a file that is not JSON at all says nothing
      // useful through the parser's message.
      showToast(t("tools.notAnExport"));
      return;
    }
    await run(
      () => importLibrary({ ...doc, download: withAudio }, onUnauthorized),
      t("tools.imported"),
    );
  }

  const orphans = status?.orphans.length ?? 0;
  const missing = status?.missing.length ?? 0;

  return (
    <div className="tools-section">
      <button className="tools-toggle" onClick={toggle} aria-expanded={open}>
        <span className="section-label">{t("tools.label")}</span>
        <span className={`tools-caret${open ? " open" : ""}`}>▾</span>
      </button>

      {open && (
        <div className="tools-body">
          <div className="tools-row">
            <span className="tools-stat">
              {status
                ? tFmt("tools.cacheSummary", {
                    size: formatBytes(status.total_bytes),
                    count: status.file_count,
                  })
                : t("tools.loading")}
            </span>
            <button className="text-btn" onClick={() => void load()} disabled={busy}>
              {t("tools.refresh")}
            </button>
          </div>

          <div className="tools-actions">
            <button
              className="btn btn-outline btn-sm"
              disabled={busy || orphans === 0}
              onClick={() =>
                void run(() => cleanupCache(onUnauthorized), t("tools.cleaned"))
              }
            >
              {tFmt("tools.cleanup", { count: orphans })}
            </button>
            <button
              className="btn btn-outline btn-sm"
              disabled={busy || missing === 0}
              onClick={() =>
                void run(() => repairCache(onUnauthorized), t("tools.repairStarted"))
              }
            >
              {tFmt("tools.repair", { count: missing })}
            </button>
          </div>

          {status && (
            <div className="tools-hint">
              {orphans === 0 && missing === 0 ? t("tools.cacheHealthy") : t("tools.cacheHint")}
            </div>
          )}

          <div className="tools-actions">
            <button className="btn btn-outline btn-sm" disabled={busy} onClick={() => void save()}>
              {t("tools.export")}
            </button>
            <label className={`btn btn-outline btn-sm tools-file${busy ? " disabled" : ""}`}>
              {t("tools.import")}
              <input
                type="file"
                accept="application/json,.json"
                disabled={busy}
                onChange={(e) => {
                  const file = e.target.files?.[0];
                  // Cleared so picking the same file again still fires a change
                  e.target.value = "";
                  if (file) void restore(file);
                }}
              />
            </label>
          </div>

          <label className="tools-check">
            <input
              type="checkbox"
              checked={withAudio}
              onChange={(e) => setWithAudio(e.target.checked)}
            />
            {t("tools.importWithAudio")}
          </label>
        </div>
      )}
    </div>
  );
}
