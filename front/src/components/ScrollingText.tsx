import { useLayoutEffect, useRef, useState } from "react";

// はみ出し分をスクロールする速度 (px/s)
const SCROLL_SPEED = 30;
// タイムライン中でスクロールに使う割合 (残りは両端での停止時間)
const SCROLL_RATIO = 0.66;
// 右端フェードマスクの幅 (styles.css の .marquee.overflowing と合わせる)。
// この分だけ余計にスクロールし、終端で末尾の文字がフェードにかからないようにする
const FADE_WIDTH = 16;

interface Props {
  text: string;
  className?: string;
}

/**
 * コンテナ幅に収まらないテキストを Spotify 風に往復スクロールさせる。
 * 幅は常にコンテナ側で決まり、テキスト長でレイアウトが広がることはない。
 */
export function ScrollingText({ text, className }: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  const textRef = useRef<HTMLSpanElement>(null);
  const [distance, setDistance] = useState(0);

  useLayoutEffect(() => {
    const container = containerRef.current;
    const inner = textRef.current;
    if (!container || !inner) return;

    const measure = () => {
      const diff = inner.scrollWidth - container.clientWidth;
      setDistance(diff > 1 ? diff + FADE_WIDTH : 0);
    };
    measure();

    const observer = new ResizeObserver(measure);
    observer.observe(container);
    return () => observer.disconnect();
  }, [text]);

  const overflowing = distance > 0;
  const duration = Math.max(6, (distance / SCROLL_SPEED) * 2 / SCROLL_RATIO);

  return (
    <div
      ref={containerRef}
      className={`marquee${overflowing ? " overflowing" : ""}${className ? ` ${className}` : ""}`}
    >
      <span
        ref={textRef}
        className="marquee-text"
        style={
          overflowing
            ? ({
                "--marquee-shift": `-${distance}px`,
                "--marquee-duration": `${duration.toFixed(2)}s`,
              } as React.CSSProperties)
            : undefined
        }
      >
        {text}
      </span>
    </div>
  );
}
