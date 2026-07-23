import { t } from "../i18n";
import type { DownloadProgress } from "../types";

interface Props {
  downloads: DownloadProgress[];
  onCancel: () => void;
}

function statusText(d: DownloadProgress): string {
  switch (d.status) {
    case "downloading":
      return `${Math.floor(d.percent)}%`;
    case "metadata":
      return t("download.metadata");
    case "processing":
      return t("download.processing");
    case "error":
      return t("download.error");
    default:
      return d.status;
  }
}

export function DownloadList({ downloads, onCancel }: Props) {
  if (downloads.length === 0) return null;

  const hasActive = downloads.some((d) => d.status !== "error");

  return (
    <div className="downloads-section">
      {hasActive && (
        <div className="downloads-header">
          <button className="text-btn text-btn-danger" onClick={onCancel}>
            {t("download.cancel")}
          </button>
        </div>
      )}
      {downloads.map((d) => {
        const indeterminate = d.status === "metadata" || d.status === "processing";
        return (
          <div
            key={d.id}
            className={`download-item${d.status === "error" ? " error" : ""}`}
          >
            <div className="download-row">
              <span className="download-title">{d.title}</span>
              <span className="download-status">{statusText(d)}</span>
            </div>
            {d.status === "error" ? (
              <div className="download-error-text">{d.error}</div>
            ) : (
              <div className={`download-bar${indeterminate ? " indeterminate" : ""}`}>
                <div
                  className="download-bar-fill"
                  style={indeterminate ? undefined : { width: `${d.percent}%` }}
                />
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
