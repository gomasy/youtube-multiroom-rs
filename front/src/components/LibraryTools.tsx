import { useCallback, useState } from "react";
import { cleanupCache, fetchCacheStatus, repairCache } from "../api";
import { showApiError } from "../errors";
import { formatBytes } from "../format";
import { t, tFmt } from "../i18n";
import type { CacheStatus } from "../types";

interface Props {
  onUnauthorized: () => void;
  showToast: (msg: string) => void;
}

/**
 * Cache upkeep: what the cache holds, what it holds for nothing, and what the
 * library expects of it that is no longer there.
 *
 * Collapsed until asked for, which is also what keeps the cache scan off the
 * page load — it walks the whole cache directory, and nothing here changes
 * often enough to be worth watching.
 */
export function LibraryTools({ onUnauthorized, showToast }: Props) {
  const [open, setOpen] = useState(false);
  const [status, setStatus] = useState<CacheStatus | null>(null);
  const [busy, setBusy] = useState(false);

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

  /** Run an action that reports a message and changes what the panel shows. */
  async function run(action: () => Promise<{ message?: string }>, fallback: string) {
    setBusy(true);
    try {
      const data = await action();
      showToast(data.message || fallback);
      await load();
    } catch (e) {
      showApiError(showToast, e);
    } finally {
      setBusy(false);
    }
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
        </div>
      )}
    </div>
  );
}
