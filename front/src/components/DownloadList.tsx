import type { DownloadProgress } from "../types";

interface Props {
  downloads: DownloadProgress[];
}

/** 行の右側に出す状態表示。網羅 switch なので status の追加漏れは型エラーになる */
function statusText(d: DownloadProgress): string {
  switch (d.status) {
    case "downloading":
      return `${Math.floor(d.percent)}%`;
    case "metadata":
      return "情報取得中...";
    case "processing":
      return "変換中...";
    case "error":
      return "エラー";
    default:
      // サーバーとのバージョン差で未知の status が来ても空欄にしない
      return d.status;
  }
}

/**
 * 進行中ダウンロードの進捗一覧。サーバー側で管理される状態を映すだけ
 * なので、リロードしても他のブラウザからでも同じ進捗が見える
 */
export function DownloadList({ downloads }: Props) {
  if (downloads.length === 0) return null;

  return (
    <div className="downloads-section">
      {downloads.map((d) => {
        // ダウンロード中以外は割合が定まらないため流れるバーで示す
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
