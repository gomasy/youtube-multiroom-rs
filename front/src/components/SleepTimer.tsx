import { useEffect, useState } from "react";
import { formatTime } from "../format";
import { t, tFmt } from "../i18n";

const PRESETS = [15, 30, 60, 180, 360];

interface Props {
  expiresAt: number | null;
  onSet: (minutes: number) => void;
  onCancel: () => void;
}

export function SleepTimer({ expiresAt, onSet, onCancel }: Props) {
  const [now, setNow] = useState(() => Date.now() / 1000);

  useEffect(() => {
    if (!expiresAt) return;
    const timer = setInterval(() => setNow(Date.now() / 1000), 1000);
    return () => clearInterval(timer);
  }, [expiresAt]);

  const remaining = expiresAt ? Math.max(0, expiresAt - now) : 0;
  const active = expiresAt !== null && remaining > 0;

  return (
    <div className="sleep-timer-section">
      <div className="section-label">{t("sleep.label")}</div>
      {active ? (
        <div className="sleep-active">
          <span className="sleep-remaining">{formatTime(remaining)}</span>
          <button className="text-btn text-btn-danger" onClick={onCancel}>
            {t("sleep.cancel")}
          </button>
        </div>
      ) : (
        <select
          className="select sleep-select"
          value=""
          onChange={(e) => {
            const minutes = Number(e.target.value);
            if (minutes > 0) onSet(minutes);
          }}
        >
          <option value="" disabled>
            {t("sleep.selectTime")}
          </option>
          {PRESETS.map((m) => (
            <option key={m} value={m}>
              {m >= 60 && m % 60 === 0
                ? tFmt("sleep.hours", { h: m / 60 })
                : tFmt("sleep.minutes", { m })}
            </option>
          ))}
        </select>
      )}
    </div>
  );
}
