interface Props {
  connected: boolean;
}

export function Header({ connected }: Props) {
  return (
    <div className="header">
      <svg width="28" height="28" viewBox="0 0 24 24" fill="none">
        <rect width="24" height="24" rx="6" fill="var(--accent)" />
        <polygon points="10,7 10,17 17,12" fill="#fff" />
      </svg>
      <h1>YouTube MultiRoom</h1>
      <div
        className={`dot${connected ? "" : " disconnected"}`}
        title={connected ? "サーバー接続中" : "切断中"}
      />
    </div>
  );
}
