import React, { useMemo } from 'react';
import { clusterSearchTimelineMarkers } from '../../lib/timeline_search';

/** Search-hit overlay shared by every timeline zoom tier. */
function markerTitle(cluster) {
  const from = new Date(cluster.from).toLocaleString();
  if (cluster.count === 1) return from;
  const to = new Date(cluster.to).toLocaleString();
  return `${from} - ${to} (${cluster.count})`;
}

export default function SearchMarkerLayer({
  markers,
  hoveredIds,
  timeToX,
  width,
  height,
  overscanPx = 0,
  stageRef,
  compact = false,
  onSelectMarker,
}) {
  const clusters = useMemo(
    () => clusterSearchTimelineMarkers(markers, timeToX, width, hoveredIds, undefined, overscanPx),
    [markers, hoveredIds, timeToX, width, overscanPx],
  );

  if (clusters.length === 0) return null;

  return (
    <div className="pointer-events-none absolute inset-x-0 top-0 z-[25] overflow-hidden" style={{ height }} aria-hidden="true">
      <div
        ref={stageRef}
        className="absolute inset-0"
        style={{ willChange: stageRef ? 'transform' : undefined }}
      >
        {clusters.map((cluster) => (
          <div
            key={cluster.key}
            // Only the overview band eases its markers into place. On the main
            // track the camera already carries them, and a transition layered on
            // top of that would show every marker moving twice.
            //
            // A marker stands on a single moment rather than over a span, so the
            // camera's horizontal scale is undone about that moment: the diamond
            // keeps its corners and the count keeps its shape, while the anchor
            // itself stays exactly where `left` puts it.
            className={`tl-steady tl-steady-left absolute top-0 ${compact ? 'transition-[left] duration-200 ease-out' : ''}`}
            style={{ left: cluster.x }}
          >
            {compact ? (
              <span
                className={`absolute left-0 top-0 block -translate-x-1/2 rounded-full transition-all duration-150 ${
                  cluster.active ? 'w-1.5 shadow-[0_0_8px_rgba(245,158,11,0.95)]' : 'w-0.5 opacity-80'
                }`}
                style={{ height, backgroundColor: 'var(--tl-search)' }}
              />
            ) : (
              <button
                type="button"
                data-timeline-item="search-marker"
                className="pointer-events-auto absolute left-0 top-0 block h-full w-4 -translate-x-1/2 cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-warning"
                style={{ height }}
                title={markerTitle(cluster)}
                aria-label={markerTitle(cluster)}
                onMouseDown={(event) => event.stopPropagation()}
                onClick={(event) => {
                  event.stopPropagation();
                  onSelectMarker?.(cluster.representative);
                }}
              >
                <span
                  className={`absolute left-1/2 top-1 block -translate-x-1/2 rotate-45 border border-white/70 transition-all duration-150 ${
                    cluster.active ? 'h-2.5 w-2.5 scale-125 shadow-[0_0_10px_rgba(245,158,11,0.95)]' : 'h-2 w-2'
                  }`}
                  style={{ backgroundColor: 'var(--tl-search)' }}
                />
                <span
                  className={`absolute left-1/2 top-2 block -translate-x-1/2 transition-all duration-150 ${
                    cluster.active ? 'w-0.5 opacity-100 shadow-[0_0_8px_rgba(245,158,11,0.9)]' : 'w-px opacity-70'
                  }`}
                  style={{ height: Math.max(0, height - 10), backgroundColor: 'var(--tl-search)' }}
                />
                {cluster.count > 1 && (
                  <span
                    className={`absolute left-[calc(50%+6px)] top-2 min-w-4 rounded-sm px-1 text-center font-mono text-[9px] font-semibold leading-4 text-white shadow-sm transition-all ${
                      cluster.active ? 'scale-110' : ''
                    }`}
                    style={{ backgroundColor: 'var(--tl-search)' }}
                  >
                    {cluster.count}
                  </span>
                )}
              </button>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}
