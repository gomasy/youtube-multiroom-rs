import { useState, useCallback, useRef, useEffect } from "react";

interface ToastItem {
  id: number;
  message: string;
}

let nextId = 0;

export function useToast() {
  const [toasts, setToasts] = useState<ToastItem[]>([]);
  /**
   * Dismissal timers still pending. Tracked so unmounting cancels them instead
   * of leaving each outstanding toast holding a timer for its full duration.
   */
  const timers = useRef(new Set<ReturnType<typeof setTimeout>>());

  useEffect(
    () => () => {
      for (const timer of timers.current) clearTimeout(timer);
      timers.current.clear();
    },
    [],
  );

  const showToast = useCallback((message: string, duration = 4000) => {
    const id = nextId++;
    setToasts((prev) => [...prev, { id, message }]);
    const timer = setTimeout(() => {
      timers.current.delete(timer);
      setToasts((prev) => prev.filter((t) => t.id !== id));
    }, duration);
    timers.current.add(timer);
  }, []);

  return { toasts, showToast };
}

interface Props {
  toasts: ToastItem[];
}

export function ToastContainer({ toasts }: Props) {
  return (
    <div className="toast-container">
      {toasts.map((t) => (
        <FadingToast key={t.id} message={t.message} />
      ))}
    </div>
  );
}

function FadingToast({ message }: { message: string }) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const timeout = setTimeout(() => {
      el.style.opacity = "0";
      el.style.transition = "opacity 0.3s";
    }, 3700);
    return () => clearTimeout(timeout);
  }, []);

  return (
    <div ref={ref} className="toast">
      {message}
    </div>
  );
}
