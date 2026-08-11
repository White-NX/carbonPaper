import React, { useEffect, useMemo, useRef } from 'react';
import { accentWithAlpha, getProcessColor, inkAlpha } from '../../lib/timeline_palette';
import { CATEGORY_COLORS } from '../../lib/categories';

const THUMB_SIZE = 50;
const THUMB_GAP = 3;
/** Minimum spacing between adjacent frame thumbnails. */
const MIN_FRAME_GAP = THUMB_SIZE + THUMB_GAP;

/** Thumbnail strip for fine-grained frame navigation (< 30 min view). */
function FrameStrip({
  events,
  timeToX,
  width,
  height,
  highlightedEventId,
  imageCache,
  imageEpoch,
  onSelectEvent,
  onVisibleFramesChange,
}) {
  const visibleFrames = useMemo(() => {
    const picked = [];
    let lastX = -Infinity;

    for (const event of events) {
      const x = timeToX(event.timestamp);
      if (x < -THUMB_SIZE) continue;
      if (x > width + THUMB_SIZE) break;

      const isHighlighted = highlightedEventId === event.id;
      if (!isHighlighted && x - lastX < MIN_FRAME_GAP) continue;

      picked.push({ event, x });
      lastX = x;
    }
    return picked;
  }, [events, timeToX, width, highlightedEventId]);

  useEffect(() => {
    const ids = visibleFrames
      .map(({ event }) => event.id)
      .filter((id) => typeof id === 'number' && id > 0);
    onVisibleFramesChange?.(ids);
  }, [visibleFrames, onVisibleFramesChange]);

  const top = Math.max(0, (height - THUMB_SIZE) / 2);

  return (
    <>
      {visibleFrames.map(({ event, x }) => {
        const cacheKey = event.id ?? event.imagePath;
        const imageUrl = cacheKey !== null && cacheKey !== undefined
          ? imageCache.get(cacheKey)
          : null;
        const isHighlighted = highlightedEventId === event.id;

        return (
          <div
            key={event.id ?? `${event.timestamp}-${x}`}
            data-timeline-item="frame"
            className={`absolute cursor-pointer overflow-hidden rounded-sm border transition-transform hover:scale-110 ${
              isHighlighted
                ? 'z-20 scale-110 border-ide-accent ring-1 ring-ide-accent'
                : 'z-10 border-ide-border'
            }`}
            style={{
              left: x - THUMB_SIZE / 2,
              top,
              width: THUMB_SIZE,
              height: THUMB_SIZE,
            }}
            onClick={(clickEvent) => {
              clickEvent.stopPropagation();
              onSelectEvent?.(event);
            }}
            title={new Date(event.timestamp).toLocaleString()}
          >
            <div className="relative h-full w-full bg-ide-active">
              {imageUrl && (
                <img
                  src={imageUrl}
                  alt=""
                  className="pointer-events-none h-full w-full object-cover"
                  data-epoch={imageEpoch}
                />
              )}
              {event.category && event.category !== '未分类' && (
                <span
                  className="absolute inset-x-0 bottom-0 h-1"
                  style={{ backgroundColor: CATEGORY_COLORS[event.category] || '#6b7280' }}
                />
              )}
            </div>
          </div>
        );
      })}
    </>
  );
}

/** Density notch strip for medium zoom span (30 min - 8 hours). */
function NotchStrip({ buckets, timeToX, width, height }) {
  const canvasRef = useRef(null);

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
    const baseline = height - 4;

    for (const bucket of buckets) {
      const x = timeToX(bucket.timestamp);
      const nextX = timeToX(bucket.timestamp + bucketMs);
      const barWidth = Math.max(1, nextX - x - 0.5);
      if (x + barWidth < 0 || x > width) continue;

      const ratio = bucket.count / maxCount;
      const barHeight = Math.max(3, ratio * (height - 10));
      ctx.fillStyle = accentWithAlpha(inkAlpha(ratio));
      ctx.fillRect(x, baseline - barHeight, barWidth, barHeight);
    }
  }, [buckets, timeToX, width, height]);

  return <canvas ref={canvasRef} className="pointer-events-none absolute inset-0" />;
}

/** Stacked app distribution bars for wide zoom span (> 8 hours). */
function StackedBars({ distribution, timeToX, width, height, onSeek }) {
  if (!distribution || distribution.length === 0) return null;

  const maxCount = distribution.reduce((max, bucket) => Math.max(max, bucket.count), 0);
  if (maxCount <= 0) return null;

  const bucketMs = distribution[0].bucketMs || 1;
  const usableHeight = height - 6;

  return (
    <>
      {distribution.map((bucket) => {
        const x = timeToX(bucket.timestamp);
        const nextX = timeToX(bucket.timestamp + bucketMs);
        const barWidth = Math.max(1, nextX - x - 2);
        if (x + barWidth < 0 || x > width) return null;

        const barHeight = Math.max(2, (bucket.count / maxCount) * usableHeight);

        return (
          <div
            key={bucket.timestamp}
            data-timeline-item="bar"
            className="absolute bottom-1 flex cursor-pointer flex-col-reverse overflow-hidden rounded-sm"
            style={{ left: x + 1, width: barWidth, height: barHeight }}
            title={`${new Date(bucket.timestamp).toLocaleDateString()} · ${bucket.count}`}
            onClick={(event) => {
              event.stopPropagation();
              onSeek?.(bucket.timestamp + bucketMs / 2);
            }}
          >
            {bucket.slices.length > 0 ? (
              bucket.slices.map((slice, index) => (
                <span
                  key={`${bucket.timestamp}-${slice.appName ?? 'other'}-${index}`}
                  style={{
                    height: `${slice.ratio * 100}%`,
                    backgroundColor: slice.appName
                      ? getProcessColor(slice.appName)
                      : 'var(--tl-app-muted)',
                  }}
                  title={slice.appName || undefined}
                />
              ))
            ) : (
              <span className="h-full" style={{ backgroundColor: accentWithAlpha(0.38) }} />
            )}
          </div>
        );
      })}
    </>
  );
}

/** Detail track component that toggles representation based on current zoom tier. */
export default function DetailTrack({
  tier,
  events,
  buckets,
  distribution,
  timeToX,
  width,
  height,
  highlightedEventId,
  imageCache,
  imageEpoch,
  onSelectEvent,
  onSeek,
  onVisibleFramesChange,
}) {
  return (
    <div
      className="relative shrink-0 overflow-hidden border-b border-ide-border"
      style={{ height }}
      data-keep-selection="true"
    >
      {tier === 'frame' && (
        <FrameStrip
          events={events}
          timeToX={timeToX}
          width={width}
          height={height}
          highlightedEventId={highlightedEventId}
          imageCache={imageCache}
          imageEpoch={imageEpoch}
          onSelectEvent={onSelectEvent}
          onVisibleFramesChange={onVisibleFramesChange}
        />
      )}

      {tier === 'session' && (
        <NotchStrip
          buckets={buckets}
          timeToX={timeToX}
          width={width}
          height={height}
        />
      )}

      {tier === 'day' && (
        <StackedBars
          distribution={distribution}
          timeToX={timeToX}
          width={width}
          height={height}
          onSeek={onSeek}
        />
      )}
    </div>
  );
}
