import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ChevronDown, Clock3 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Button, focusStyle } from '../ui/Button';
import { clusterSearchTimelineMarkers, getSearchMarkerFitRange } from '../../lib/timeline_search';

const EMPTY_MARKERS = [];
const SCRUB_STEPS = 1000;
const MARKER_TARGET_PX = 28;

/** A search overview with its own scale; the expanded camera keeps its viewport. */
export default function CompactTimeline({
  markers = EMPTY_MARKERS,
  hoveredIds = EMPTY_MARKERS,
  centerTime,
  onScrub,
  onSelectMarker,
  onExpand,
}) {
  const { t, i18n } = useTranslation();
  const railRef = useRef(null);
  const [width, setWidth] = useState(0);
  const [targetPx, setTargetPx] = useState(MARKER_TARGET_PX);
  const range = useMemo(() => getSearchMarkerFitRange(markers), [markers]);
  const hasMarkers = Boolean(range);
  const span = range ? range.to - range.from : 1;
  const timeToX = useCallback(
    (time) => range ? ((time - range.from) / span) * width : 0,
    [range, span, width],
  );
  const clusters = useMemo(
    () => clusterSearchTimelineMarkers(markers, timeToX, width, hoveredIds, targetPx),
    [markers, timeToX, width, hoveredIds, targetPx],
  );
  const formatters = useMemo(() => ({
    time: new Intl.DateTimeFormat(i18n.language, { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' }),
    date: new Intl.DateTimeFormat(i18n.language, { month: 'short', day: 'numeric' }),
    full: new Intl.DateTimeFormat(i18n.language, { year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' }),
  }), [i18n.language]);

  useEffect(() => {
    const rail = railRef.current;
    if (!hasMarkers || !rail) return undefined;
    setWidth(rail.clientWidth);
    const observer = new ResizeObserver(([entry]) => {
      if (entry?.contentRect.width > 0) setWidth(entry.contentRect.width);
    });
    observer.observe(rail);
    return () => observer.disconnect();
  }, [hasMarkers]);

  useEffect(() => {
    const query = window.matchMedia?.('(pointer: coarse)');
    if (!query) return undefined;
    const update = () => setTargetPx(query.matches ? 44 : MARKER_TARGET_PX);
    update();
    query.addEventListener?.('change', update);
    return () => query.removeEventListener?.('change', update);
  }, []);

  const inRange = range && centerTime >= range.from && centerTime <= range.to;
  const scrubTime = range ? Math.max(range.from, Math.min(range.to, centerTime)) : centerTime;
  const scrubValue = range ? ((scrubTime - range.from) / span) * SCRUB_STEPS : 0;
  const sameDay = range && new Date(range.from).toDateString() === new Date(range.to).toDateString();
  const rangeFormatter = sameDay ? formatters.time : formatters.date;

  return (
    <div className="tl-compact-bar" data-has-markers={hasMarkers} data-keep-selection="true">
      <div className="tl-compact-context">
        <Clock3 className="h-3.5 w-3.5 shrink-0 text-ide-muted" aria-hidden="true" />
        <span>{t('timeline.title')}</span>
        <time className="tl-compact-time" dateTime={new Date(centerTime).toISOString()} title={formatters.full.format(centerTime)}>
          {hasMarkers ? formatters.time.format(centerTime) : formatters.date.format(centerTime)}
        </time>
      </div>

      {range && (
        <div className="tl-compact-rail" ref={railRef}>
          <input
            className="tl-compact-scrubber"
            type="range"
            min={0}
            max={SCRUB_STEPS}
            step={1}
            value={scrubValue}
            aria-label={t('timeline.searchResultsTrack')}
            aria-valuetext={formatters.full.format(scrubTime)}
            data-in-range={Boolean(inRange)}
            onChange={(event) => onScrub?.(range.from + (Number(event.target.value) / SCRUB_STEPS) * span)}
          />
          <div className="tl-compact-markers">
            {clusters.map((cluster, index) => {
              // Split neighboring hit areas at their midpoint so dense markers
              // cannot intercept one another's clicks.
              const left = Math.max(0, cluster.x - targetPx / 2,
                index > 0 ? (clusters[index - 1].x + cluster.x) / 2 : 0);
              const right = Math.min(width, cluster.x + targetPx / 2,
                index < clusters.length - 1 ? (cluster.x + clusters[index + 1].x) / 2 : width);
              const selected = centerTime >= cluster.from && centerTime <= cluster.to;
              const label = cluster.count === 1
                ? t('timeline.searchResultAt', { time: formatters.full.format(cluster.from) })
                : t('timeline.searchResultsBetween', {
                  from: formatters.full.format(cluster.from), to: formatters.full.format(cluster.to), count: cluster.count,
                });
              return (
                <button
                  key={cluster.key}
                  type="button"
                  className={`tl-compact-marker ${focusStyle}`}
                  style={{ left, width: right - left }}
                  title={label}
                  aria-label={label}
                  aria-pressed={selected}
                  data-active={selected || cluster.active}
                  data-selected={selected}
                  onClick={() => onSelectMarker?.(cluster.representative)}
                >
                  <span className="tl-compact-marker-dot" style={{ left: cluster.x - left }} />
                </button>
              );
            })}
          </div>
          <div className="tl-compact-range" aria-hidden="true">
            <span>{rangeFormatter.format(range.from)}</span>
            <span>{rangeFormatter.format(range.to)}</span>
          </div>
        </div>
      )}

      {onExpand && (
        <Button
          variant="secondary"
          size="xs"
          icon={ChevronDown}
          className="tl-compact-expand"
          aria-expanded={false}
          data-tauri-drag-region="false"
          onClick={onExpand}
        >
          {t('timeline.expand')}
        </Button>
      )}
    </div>
  );
}
