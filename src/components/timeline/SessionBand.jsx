import React, { useCallback, useMemo, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { getProcessColor, stripePattern, withAlpha } from '../../lib/timeline_palette';

/** Minimum rendered width for a session block in pixels. */
const MIN_RENDER_PX = 1;
const SHOW_ICON_PX = 26;
const SHOW_NAME_PX = 84;
const SHOW_DURATION_PX = 132;
const SHOW_DETAIL_PX = 200;

/** Gap between adjacent app session blocks in pixels. */
const APP_GAP_PX = 2;

/** Tick rendering limits inside a block. */
const MIN_TICK_GAP_PX = 4;
const MAX_TICKS = 80;

/**
 * Opacity for internal window switch ticks and foreign app interruptions.
 *
 * These ticks are the main signal that a block is standing in for several
 * activities, so they are drawn firmly enough to read at a glance. A block that
 * looks atomic is exactly what makes it startling when zooming splits it apart.
 */
const TICK_ALPHA = 0.34;
const FOREIGN_TICK_ALPHA = 0.62;

/** Minimum block width in pixels before the folded-block hint has room. */
const FOLD_HINT_MIN_PX = 14;
/** Opacity of the folded-block hint strokes. */
const FOLD_HINT_ALPHA = 0.5;

/** Max stacked app icons for mixed blocks. */
const MAX_STACKED_ICONS = 3;
/** Horizontal spacing increment per stacked icon. */
const STACKED_ICON_STEP_PX = 11;
/** Max colors rendered in mixed block stripes. */
const MIXED_STRIPE_COLORS = 3;

/** Max titles listed in tooltip. */
const MAX_TOOLTIP_TITLES = 6;

function iconSource(processIcon) {
  if (!processIcon) return null;
  return processIcon.startsWith('data:') ? processIcon : `data:image/png;base64,${processIcon}`;
}

/**
 * Build tick marks for window switches and interruptions within a session block.
 *
 * A heavily folded block can hold thousands of segments while only a handful of
 * ticks physically fit, so the segments are walked with a stride rather than
 * scanned one by one on every frame. The ticks that survive are then spread over
 * the whole block instead of crowding into its left edge.
 */
function buildTicks(block, timeToX, blockLeft, blockWidth) {
  const ticks = [];
  const capacity = Math.min(MAX_TICKS, Math.floor(blockWidth / MIN_TICK_GAP_PX));
  if (capacity <= 0) return ticks;

  const stride = Math.max(1, Math.ceil((block.segments.length - 1) / capacity));
  let lastX = -Infinity;

  for (let i = 1; i < block.segments.length; i += stride) {
    const segment = block.segments[i];
    const x = timeToX(segment.start) - blockLeft;
    // Ignore ticks too close to block boundaries
    if (x < 3 || x > blockWidth - 2) continue;
    if (x - lastX < MIN_TICK_GAP_PX) continue;

    lastX = x;
    ticks.push({
      x,
      key: `${segment.key}-${segment.start}`,
      foreign: block.kind === 'app' && segment.appName !== block.appName,
      appName: segment.appName,
    });
  }

  return ticks;
}

/**
 * Compose the hover tooltip for a block.
 *
 * @param {(count: number) => string} describeSegments Localized segment count
 */
function buildTooltip(block, describeSegments) {
  const lines = [];

  if (block.kind === 'mixed') {
    for (const app of block.apps || []) {
      lines.push(`${app.appName || '?'} ${Math.round(app.ratio * 100)}%`);
    }
  } else {
    if (block.appName) lines.push(block.appName);

    const seen = new Set();
    for (const segment of block.segments) {
      const title = segment.windowTitle;
      if (!title || seen.has(title)) continue;
      seen.add(title);
      lines.push(segment.appName === block.appName ? title : `${segment.appName} — ${title}`);
      if (seen.size >= MAX_TOOLTIP_TITLES) break;
    }
  }

  // State plainly how much the block stands in for, so that an expand on zoom is
  // something the reader was already told to expect.
  if (block.segments.length > 1) lines.push(describeSegments(block.segments.length));

  return lines.join('\n');
}

/** Session band component displaying consolidated application activity blocks. */
export default function SessionBand({
  blocks,
  timeToX,
  width,
  height,
  overscanPx = 0,
  foldLevel = null,
  stageRef,
  activeId,
  onSelect,
  onZoomTo,
}) {
  const { t } = useTranslation();

  /**
   * Blocks that appeared because the fold level changed, rather than because the
   * band scrolled or new data arrived.
   *
   * Block identity survives folding, so the block a zoom splits apart keeps its
   * element and stays exactly where it was while the pieces it was standing in
   * for fade in beside it. That is the whole effect: nothing jumps, something
   * opens.
   *
   * The answer is remembered for as long as the level holds, so that recomputing
   * it returns the same set instead of an empty one. React invokes this twice
   * per render in development, and the fade would otherwise exist only in
   * production.
   */
  const foldStateRef = useRef({ level: null, ids: null, fresh: null });
  const freshIds = useMemo(() => {
    const state = foldStateRef.current;
    const ids = new Set(blocks.map((block) => block.id));

    if (state.level === foldLevel) {
      state.ids = ids;
      return state.fresh;
    }

    const fresh = state.ids
      ? new Set([...ids].filter((id) => !state.ids.has(id)))
      : null;

    foldStateRef.current = { level: foldLevel, ids, fresh };
    return fresh;
  }, [blocks, foldLevel]);

  const formatDuration = useCallback((ms) => {
    const totalSeconds = Math.max(0, Math.round(ms / 1000));
    if (totalSeconds < 60) {
      return t('timeline.duration.seconds', { seconds: totalSeconds });
    }
    const totalMinutes = Math.floor(totalSeconds / 60);
    if (totalMinutes < 60) {
      return t('timeline.duration.minutes', { minutes: totalMinutes });
    }
    const hours = Math.floor(totalMinutes / 60);
    const minutes = totalMinutes % 60;
    return t('timeline.duration.hours', { hours, minutes });
  }, [t]);

  /** Localized count of the activity segments a block stands in for. */
  const describeSegments = useCallback(
    (segmentCount) => t('timeline.session.segments', { segmentCount }),
    [t],
  );

  /** Format supplementary detail description for a block. */
  const describeBlock = useCallback((block) => {
    if (block.kind === 'mixed') {
      return t('timeline.session.switches', { switchCount: block.switches });
    }
    if (block.kind === 'activity') return block.windowTitle || '';

    const parts = [];
    if (block.windowCount > 1) {
      parts.push(t('timeline.session.windows', { windowCount: block.windowCount }));
    }
    if (block.interruptions > 0) {
      parts.push(t('timeline.session.interruptions', { interruptionCount: block.interruptions }));
    }
    return parts.join(' · ');
  }, [t]);

  return (
    <div
      className="relative shrink-0 overflow-hidden border-b border-ide-border bg-ide-panel"
      style={{ height }}
      data-keep-selection="true"
    >
      <div ref={stageRef} className="absolute inset-0" style={{ willChange: 'transform' }}>
        {blocks.map((block, index) => {
          const left = timeToX(block.start);
          const right = timeToX(block.end);
          if (right < -overscanPx || left > width + overscanPx) return null;

          // Adjust for blocks partially outside the drawn area
          const visibleLeft = Math.max(-overscanPx, left);
          const visibleWidth = Math.min(width + overscanPx, right) - visibleLeft;
          if (visibleWidth <= 0) return null;

          const previous = blocks[index - 1] || null;
          const next = blocks[index + 1] || null;
          // Join adjacent blocks of the same app seamlessly
          const joinLeft = Boolean(block.appName) && previous?.appName === block.appName;
          const joinRight = Boolean(block.appName) && next?.appName === block.appName;

          const isMixed = block.kind === 'mixed';
          const isActive = activeId !== null && activeId === block.id;
          const color = isMixed ? null : getProcessColor(block.appName);

          // No gap when clipped at the right edge
          const gap = joinRight || right > width + overscanPx ? 0 : APP_GAP_PX;
          const renderWidth = Math.max(MIN_RENDER_PX, visibleWidth - gap);

          const background = isMixed
            ? stripePattern(
              (block.apps || [])
                .slice(0, MIXED_STRIPE_COLORS)
                .map((app) => getProcessColor(app.appName)),
              isActive ? 0.42 : 0.3,
            )
            : withAlpha(color, isActive ? 0.28 : 0.16);

          const showIcon = renderWidth >= SHOW_ICON_PX;
          const showName = renderWidth >= SHOW_NAME_PX;
          const showDuration = renderWidth >= SHOW_DURATION_PX;
          const showDetail = renderWidth >= SHOW_DETAIL_PX;

          // Stack icons for mixed blocks when space permits
          const stacked = isMixed
            ? (block.apps || []).slice(0, Math.max(1, Math.min(
              MAX_STACKED_ICONS,
              Math.floor((renderWidth - SHOW_ICON_PX) / STACKED_ICON_STEP_PX) + 1,
            )))
            : [];
          const hiddenApps = isMixed ? Math.max(0, block.appCount - stacked.length) : 0;

          const icon = isMixed ? null : iconSource(block.processIcon);
          const detail = describeBlock(block);
          const isFolded = block.segments.length > 1;
          const ticks = isFolded ? buildTicks(block, timeToX, visibleLeft, renderWidth) : [];

          // Announce a folded block even where it is too narrow for ticks to read,
          // so that splitting it on zoom confirms what the block already claimed.
          const showFoldHint = isFolded && renderWidth >= FOLD_HINT_MIN_PX;
          const foldHintColor = showFoldHint
            ? withAlpha(color ?? getProcessColor(block.apps?.[0]?.appName), FOLD_HINT_ALPHA)
            : null;

          return (
            <div
              key={block.id}
              data-timeline-item="session"
              className={`tl-session absolute top-0.5 bottom-0.5 flex items-center gap-1.5 overflow-hidden pl-1.5 cursor-pointer transition-colors ${
                showFoldHint ? 'pr-2.5' : 'pr-1'
              } ${isActive ? 'ring-1 ring-ide-accent' : ''} ${
                freshIds?.has(block.id) ? 'tl-fold-in' : ''
              }`}
              style={{
                left: visibleLeft,
                width: renderWidth,
                background,
                borderTopLeftRadius: joinLeft ? 0 : 2,
                borderBottomLeftRadius: joinLeft ? 0 : 2,
                borderTopRightRadius: joinRight ? 0 : 2,
                borderBottomRightRadius: joinRight ? 0 : 2,
              }}
              title={buildTooltip(block, describeSegments)}
              onClick={(event) => {
                event.stopPropagation();
                onSelect?.(block);
              }}
              onDoubleClick={(event) => {
                event.stopPropagation();
                onZoomTo?.(block);
              }}
            >
              {!isMixed && !joinLeft && (
                <span
                  className="tl-steady tl-steady-left pointer-events-none absolute left-0 top-0 bottom-0 w-0.5"
                  style={{ backgroundColor: color }}
                />
              )}

              {ticks.map((tick) => (
                <span
                  key={tick.key}
                  className={`pointer-events-none absolute ${tick.foreign ? 'top-0.5 bottom-0.5 w-0.5' : 'top-1 bottom-1 w-px'}`}
                  style={{
                    left: tick.x,
                    backgroundColor: tick.foreign
                      ? withAlpha(getProcessColor(tick.appName), FOREIGN_TICK_ALPHA)
                      : withAlpha(color ?? getProcessColor(tick.appName), TICK_ALPHA),
                  }}
                />
              ))}

              {showFoldHint && (
                <span
                  aria-hidden="true"
                  className="tl-steady tl-steady-right pointer-events-none absolute right-1 top-0 bottom-0 flex items-center gap-px"
                >
                  <span className="h-1.5 w-px" style={{ backgroundColor: foldHintColor }} />
                  <span className="h-2.5 w-px" style={{ backgroundColor: foldHintColor }} />
                  <span className="h-3.5 w-px" style={{ backgroundColor: foldHintColor }} />
                </span>
              )}

              {showIcon && (isMixed ? (
                <span className="tl-steady relative flex shrink-0 items-center">
                  {stacked.map((app, position) => {
                    const source = iconSource(app.processIcon);
                    const overlap = position === 0 ? 0 : -3;
                    return source ? (
                      <img
                        key={app.appName ?? position}
                        src={source}
                        alt=""
                        className="h-3.5 w-3.5 rounded-sm object-cover ring-1 ring-ide-panel"
                        style={{ marginLeft: overlap }}
                      />
                    ) : (
                      <span
                        key={app.appName ?? position}
                        className="h-3.5 w-3.5 rounded-sm ring-1 ring-ide-panel"
                        style={{ marginLeft: overlap, backgroundColor: getProcessColor(app.appName) }}
                      />
                    );
                  })}
                </span>
              ) : (
                icon ? (
                  <img
                    src={icon}
                    alt=""
                    className="tl-steady relative h-3.5 w-3.5 shrink-0 rounded-sm object-cover"
                  />
                ) : (
                  <span
                    className="tl-steady relative h-3 w-3 shrink-0 rounded-sm"
                    style={{ backgroundColor: color }}
                  />
                )
              ))}

              {showName && (isMixed ? (
                hiddenApps > 0 && (
                  <span className="tl-steady tl-steady-left relative shrink-0 font-mono text-[10px] leading-none text-ide-muted">
                    {`+${hiddenApps}`}
                  </span>
                )
              ) : (
                <span className="tl-steady tl-steady-left relative shrink-0 text-[11px] font-semibold leading-none text-ide-text">
                  {block.appName}
                </span>
              ))}

              {showDetail && detail && (
                // The clipping stays on the outer element, which the stage still
                // stretches, so it tracks the slot the detail actually has on
                // screen. Were the correction applied here, a track drawn wider
                // than it is now would let the text run over the duration.
                <span className="relative min-w-0 flex-1 overflow-hidden">
                  <span className="tl-steady tl-steady-left block truncate text-[10px] leading-none text-ide-muted">
                    {detail}
                  </span>
                </span>
              )}

              {showDuration && (
                <span className="tl-steady tl-steady-right relative ml-auto shrink-0 pl-1 font-mono text-[9.5px] leading-none text-ide-muted">
                  {formatDuration(block.end - block.start)}
                </span>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
