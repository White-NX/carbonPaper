import React, { useCallback, useEffect, useRef, useState } from 'react';
import { accentWithAlpha, inkAlpha } from '../../lib/timeline_palette';

/** Minimum draggable viewport width in pixels. */
const MIN_VIEWPORT_PX = 10;
/** Maximum pointer movement distance in pixels to count as a click. */
const CLICK_SLOP_PX = 4;

/** Overview band component showing macro-level density distribution with draggable viewport indicator. */
export default function OverviewBand({
  width,
  height,
  buckets,
  rangeStart,
  rangeEnd,
  viewStart,
  viewEnd,
  onSeek,
  onSeekRange,
  onInteractingChange,
}) {
  const canvasRef = useRef(null);
  const rootRef = useRef(null);
  const [drag, setDrag] = useState(null);

  const span = Math.max(1, rangeEnd - rangeStart);
  const timeToX = useCallback(
    (time) => ((time - rangeStart) / span) * width,
    [rangeStart, span, width],
  );
  const xToTime = useCallback(
    (x) => rangeStart + (x / Math.max(1, width)) * span,
    [rangeStart, span, width],
  );

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !width || !height) return;

    const dpr = window.devicePixelRatio || 1;
    if (canvas.style.width !== `${width}px`) {
      canvas.style.width = `${width}px`;
      canvas.style.height = `${height}px`;
    }
    if (canvas.width !== width * dpr || canvas.height !== height * dpr) {
      canvas.width = width * dpr;
      canvas.height = height * dpr;
    }

    const ctx = canvas.getContext('2d');
    ctx.resetTransform();
    ctx.scale(dpr, dpr);
    ctx.clearRect(0, 0, width, height);

    if (!buckets || buckets.length === 0) return;

    const maxCount = buckets.reduce((max, bucket) => Math.max(max, bucket.count), 0);
    if (maxCount <= 0) return;

    const bucketMs = buckets[0].bucketMs || 1;
    const barWidth = Math.max(1, (bucketMs / span) * width - 0.5);

    for (const bucket of buckets) {
      const x = timeToX(bucket.timestamp);
      if (x + barWidth < 0 || x > width) continue;
      const ratio = bucket.count / maxCount;
      const barHeight = Math.max(1.5, ratio * (height - 3));
      ctx.fillStyle = accentWithAlpha(inkAlpha(ratio));
      ctx.fillRect(x, height - barHeight, barWidth, barHeight);
    }
  }, [buckets, width, height, span, timeToX]);

  const localX = useCallback((event) => {
    const rect = rootRef.current?.getBoundingClientRect();
    if (!rect) return 0;
    return Math.min(Math.max(0, event.clientX - rect.left), width);
  }, [width]);

  const viewLeft = timeToX(viewStart);
  const viewWidth = Math.max(MIN_VIEWPORT_PX, timeToX(viewEnd) - viewLeft);

  const handleMouseDown = useCallback((event) => {
    event.stopPropagation();
    const x = localX(event);
    const insideViewport = x >= viewLeft && x <= viewLeft + viewWidth;
    onInteractingChange?.(true);
    setDrag({
      mode: insideViewport ? 'pan' : 'select',
      originX: x,
      currentX: x,
      grabOffset: x - viewLeft,
    });
  }, [localX, viewLeft, viewWidth, onInteractingChange]);

  useEffect(() => {
    if (!drag) return undefined;

    const handleMove = (event) => {
      const rect = rootRef.current?.getBoundingClientRect();
      if (!rect) return;
      const x = Math.min(Math.max(0, event.clientX - rect.left), width);
      setDrag((prev) => (prev ? { ...prev, currentX: x } : prev));

      if (drag.mode === 'pan') {
        const nextLeft = x - drag.grabOffset;
        const nextCenter = xToTime(nextLeft + viewWidth / 2);
        onSeek?.(nextCenter);
      }
    };

    const handleUp = (event) => {
      const rect = rootRef.current?.getBoundingClientRect();
      const x = rect
        ? Math.min(Math.max(0, event.clientX - rect.left), width)
        : drag.currentX;

      if (drag.mode === 'select') {
        if (Math.abs(x - drag.originX) < CLICK_SLOP_PX) {
          onSeek?.(xToTime(x));
        } else {
          const from = xToTime(Math.min(drag.originX, x));
          const to = xToTime(Math.max(drag.originX, x));
          onSeekRange?.(from, to);
        }
      }
      setDrag(null);
      onInteractingChange?.(false);
    };

    window.addEventListener('mousemove', handleMove);
    window.addEventListener('mouseup', handleUp);
    return () => {
      window.removeEventListener('mousemove', handleMove);
      window.removeEventListener('mouseup', handleUp);
    };
  }, [drag, width, xToTime, viewWidth, onSeek, onSeekRange, onInteractingChange]);

  const selectionLeft = drag?.mode === 'select' ? Math.min(drag.originX, drag.currentX) : 0;
  const selectionWidth = drag?.mode === 'select' ? Math.abs(drag.currentX - drag.originX) : 0;

  return (
    <div
      ref={rootRef}
      className="relative shrink-0 border-b border-ide-border cursor-pointer"
      style={{ height }}
      onMouseDown={handleMouseDown}
      data-keep-selection="true"
    >
      <canvas ref={canvasRef} className="absolute inset-0 pointer-events-none" />

      <div
        className="absolute top-0 bottom-0 rounded-sm border border-ide-accent pointer-events-none"
        style={{
          left: viewLeft,
          width: viewWidth,
          backgroundColor: accentWithAlpha(0.13),
          cursor: 'grab',
        }}
      />

      {selectionWidth > CLICK_SLOP_PX && (
        <div
          className="absolute top-0 bottom-0 border border-ide-accent pointer-events-none"
          style={{
            left: selectionLeft,
            width: selectionWidth,
            backgroundColor: accentWithAlpha(0.22),
          }}
        />
      )}
    </div>
  );
}
