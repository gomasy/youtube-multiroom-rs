interface Props {
  connected: boolean;
}

export function Header({ connected }: Props) {
  return (
    <div className="header">
      <svg width="34" height="24" viewBox="0 0 34 24" fill="none">
        <rect width="34" height="24" rx="6" fill="var(--accent)" />
        <polygon points="14,6 14,18 23,12" fill="#fff" />
      </svg>
      <h1>YouTube MultiRoom</h1>
      <div
        className={`dot${connected ? "" : " disconnected"}`}
        title={connected ? "サーバー接続中" : "切断中"}
      />
    </div>
  );
}
