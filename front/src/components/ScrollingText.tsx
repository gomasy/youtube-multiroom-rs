import { useLayoutEffect, useRef, useState } from "react";

// Scroll speed for overflowing text (px/s)
const SCROLL_SPEED = 30;
// Fraction of the animation timeline used for scrolling (rest is pause at each end)
const SCROLL_RATIO = 0.66;
// Width of the right-edge fade mask (must match .marquee.overflowing in styles.css).
// Extra scroll distance ensures trailing characters don't hide under the fade.
const FADE_WIDTH = 16;

interface Props {
  text: string;
  className?: string;
}

/**
 * Spotify-style back-and-forth scrolling for text that overflows its container.
 * Width is always determined by the container; text length never expands the layout.
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
