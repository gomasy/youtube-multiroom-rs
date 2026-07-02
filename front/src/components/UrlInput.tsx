import { useRef, useImperativeHandle, forwardRef } from "react";

export interface UrlInputHandle {
  clear: () => void;
}

interface Props {
  extracting: boolean;
  onExtract: (url: string) => void;
  showToast: (msg: string) => void;
}

export const UrlInput = forwardRef<UrlInputHandle, Props>(function UrlInput(
  { extracting, onExtract, showToast },
  ref,
) {
  const inputRef = useRef<HTMLInputElement>(null);

  useImperativeHandle(ref, () => ({
    clear: () => {
      if (inputRef.current) inputRef.current.value = "";
    },
  }));

  function extract() {
    const url = inputRef.current?.value.trim();
    if (!url) {
      showToast("URLを入力してください");
      return;
    }

    onExtract(url);
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
        <button className="btn" onClick={extract} disabled={extracting}>
          {extracting ? <><span className="spinner" />取得中</> : "取得"}
        </button>
      </div>
    </div>
  );
});
