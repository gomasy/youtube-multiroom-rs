import faviconUrl from "../favicon.svg";

interface Props {
  connected: boolean;
}

export function Header({ connected }: Props) {
  return (
    <div className="header">
      <img src={faviconUrl} width="34" height="24" alt="" />
      <h1>YouTube MultiRoom</h1>
      <div
        className={`dot${connected ? "" : " disconnected"}`}
        title={connected ? "サーバー接続中" : "切断中"}
      />
    </div>
  );
}
