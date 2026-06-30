import { useRef } from "react";
import { setToken, verifyToken } from "../api";

interface Props {
  onAuthenticated: () => void;
  showToast: (msg: string) => void;
}

export function AuthModal({ onAuthenticated, showToast }: Props) {
  const inputRef = useRef<HTMLInputElement>(null);

  async function handleSave() {
    const token = inputRef.current?.value.trim();
    if (!token) return;

    const valid = await verifyToken(token);
    if (!valid) {
      showToast("トークンが正しくありません");
      return;
    }

    setToken(token);
    onAuthenticated();
  }

  function handleKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Enter") handleSave();
  }

  return (
    <div className="auth-modal">
      <div className="auth-box">
        <h2>認証が必要です</h2>
        <p>API トークンを入力してください</p>
        <input
          ref={inputRef}
          type="password"
          className="url-input"
          placeholder="API_TOKEN"
          onKeyDown={handleKeyDown}
          autoFocus
        />
        <button className="btn" onClick={handleSave} style={{ width: "100%", marginTop: 12 }}>
          接続
        </button>
      </div>
    </div>
  );
}
