import { t } from "../i18n";
import logoUrl from "url:../icons/logo.svg";

interface Props {
  connected: boolean;
  version: string | null;
}

export function Header({ connected, version }: Props) {
  return (
    <div className="header">
      <img src={logoUrl} width="34" height="24" alt="" />
      <h1>YouTube MultiRoom</h1>
      {version && <span className="header-version">{version}</span>}
      <div
        className={`dot${connected ? "" : " disconnected"}`}
        title={connected ? t("header.connected") : t("header.disconnected")}
      />
    </div>
  );
}
