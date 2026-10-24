import React, { useEffect, useRef, useState } from 'react';
import { setPreference, usePreference } from '../lib/preference_store';
import { Button } from './ui/Button';

const SEPARATOR_WIDTH = 4;
const KEYBOARD_STEP = 16;

/** A persistent list width, bounded by the space needed to read its detail pane. */
export default function ResizableSplitPane({
  storageKey,
  separatorLabel,
  left,
  children,
  active = true,
  defaultWidth = 280,
  minWidth = 220,
  maxWidth = 420,
  minDetailWidth = 320,
}) {
  const preference = usePreference(storageKey, String(defaultWidth));
  const savedWidth = Number(preference);
  const preferredWidth = Number.isFinite(savedWidth) && savedWidth > 0 ? savedWidth : defaultWidth;
  const [containerWidth, setContainerWidth] = useState(null);
  const [draftWidth, setDraftWidth] = useState(null);
  const containerRef = useRef(null);
  const dragRef = useRef(null);
  const upperBound = Math.max(minWidth, Math.min(maxWidth, (containerWidth ?? Infinity) - minDetailWidth - SEPARATOR_WIDTH));
  const clamp = (value) => Math.max(minWidth, Math.min(upperBound, value));
  const width = clamp(draftWidth ?? preferredWidth);

  useEffect(() => {
    if (!active || !containerRef.current) return undefined;
    const measure = () => {
      const nextWidth = containerRef.current?.clientWidth;
      if (nextWidth > 0) setContainerWidth(nextWidth);
    };
    measure();
    if (typeof ResizeObserver === 'undefined') return undefined;
    const observer = new ResizeObserver(measure);
    observer.observe(containerRef.current);
    return () => observer.disconnect();
  }, [active]);

  useEffect(() => {
    if (active || !dragRef.current) return;
    const drag = dragRef.current;
    dragRef.current = null;
    setDraftWidth(null);
    if (drag.target.hasPointerCapture?.(drag.pointerId)) drag.target.releasePointerCapture(drag.pointerId);
  }, [active]);

  const finishDrag = (save) => {
    const drag = dragRef.current;
    if (!drag) return;
    dragRef.current = null;
    if (save) setPreference(storageKey, clamp(drag.width));
    setDraftWidth(null);
    if (drag.target.hasPointerCapture?.(drag.pointerId)) drag.target.releasePointerCapture(drag.pointerId);
  };

  return (
    <div ref={containerRef} className="flex min-h-0 min-w-0 flex-1 overflow-hidden">
      <div className="flex min-h-0 shrink-0 flex-col overflow-hidden" style={{ width }}>
        {left}
      </div>
      <Button
        role="separator"
        aria-label={separatorLabel}
        aria-orientation="vertical"
        aria-valuemin={minWidth}
        aria-valuemax={upperBound}
        aria-valuenow={width}
        title={separatorLabel}
        disabled={!active}
        variant="ghost"
        className="w-1 cursor-col-resize self-stretch touch-none rounded-none border-0 bg-ide-border p-0 hover:bg-ide-accent/50"
        data-tauri-drag-region="false"
        onPointerDown={(event) => {
          if (event.button !== 0 || !active) return;
          event.preventDefault();
          event.currentTarget.focus();
          event.currentTarget.setPointerCapture(event.pointerId);
          dragRef.current = { pointerId: event.pointerId, target: event.currentTarget, x: event.clientX, startWidth: width, width };
          setDraftWidth(width);
        }}
        onPointerMove={(event) => {
          const drag = dragRef.current;
          if (!drag || drag.pointerId !== event.pointerId) return;
          drag.width = clamp(drag.startWidth + event.clientX - drag.x);
          setDraftWidth(drag.width);
        }}
        onPointerUp={(event) => {
          if (dragRef.current?.pointerId === event.pointerId) finishDrag(true);
        }}
        onPointerCancel={() => finishDrag(false)}
        onLostPointerCapture={() => finishDrag(false)}
        onDoubleClick={() => setPreference(storageKey, defaultWidth)}
        onKeyDown={(event) => {
          let nextWidth;
          switch (event.key) {
            case 'ArrowLeft': nextWidth = width - KEYBOARD_STEP; break;
            case 'ArrowRight': nextWidth = width + KEYBOARD_STEP; break;
            case 'Home': nextWidth = minWidth; break;
            case 'End': nextWidth = upperBound; break;
            case 'Escape': finishDrag(false); return;
            default: return;
          }
          event.preventDefault();
          setPreference(storageKey, clamp(nextWidth));
        }}
      />
      <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">{children}</div>
    </div>
  );
}
