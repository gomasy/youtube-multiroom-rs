import { useEffect, useState } from "react";
import { t, tFmt } from "../i18n";

const PRESETS = [15, 30, 60, 90];

interface Props {
  expiresAt: number | null;
  onSet: (minutes: number) => void;
  onCancel: () => void;
}

function formatRemaining(seconds: number): string {
  if (seconds <= 0) return "0:00";
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${m}:${s.toString().padStart(2, "0")}`;
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
          <span className="sleep-remaining">{formatRemaining(remaining)}</span>
          <button className="text-btn text-btn-danger" onClick={onCancel}>
            {t("sleep.cancel")}
          </button>
        </div>
      ) : (
        <div className="sleep-presets">
          {PRESETS.map((m) => (
            <button
              key={m}
              className="btn btn-outline btn-sm"
              onClick={() => onSet(m)}
            >
              {tFmt("sleep.minutes", { m })}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
