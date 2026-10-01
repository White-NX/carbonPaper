import React, { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { Eye, Maximize2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { usePreference } from '../../lib/preference_store';
import { fetchThumbnail } from '../../lib/monitor_api';
import { sourceResult } from '../../lib/recap_api';

export function useRecapSourceAction(onSelect, onFloating) {
  const { t } = useTranslation();
  const behavior = usePreference('cardClickBehavior_recap', 'standalone');
  const floating = behavior === 'standalone' && Boolean(onFloating);
  return {
    floating,
    open: (source, alternate = false) => {
      if ((alternate ? !floating : floating) && onFloating) onFloating(sourceResult(source), { sourceLabel: t('recap.title'), sourceType: 'recap' });
      else onSelect?.(sourceResult(source));
    },
  };
}

export function AlternateSourceButton({ source, action }) {
  const { t } = useTranslation();
  const label = t(action.floating ? 'previewAction.openMainPreview' : 'previewAction.openFloatingPreview');
  const Icon = action.floating ? Eye : Maximize2;
  return <button type="button" onClick={() => action.open(source, true)} title={label} aria-label={label}
    className="recap-focus shrink-0 rounded-md border border-ide-border bg-ide-panel p-1.5 text-ide-muted opacity-0 transition-opacity hover:text-ide-text focus-visible:opacity-100 group-hover:opacity-100">
    <Icon className="h-3.5 w-3.5" />
  </button>;
}

export function SourceThumbnail({ source, action, time }) {
  const ref = useRef(null);
  const [image, setImage] = useState(null);
  useEffect(() => {
    let live = true;
    let started = false;
    const load = () => {
      if (started) return;
      started = true;
      fetchThumbnail(source.id, null).then((result) => { if (live) setImage(result); }).catch(() => {});
    };
    if (typeof IntersectionObserver === 'undefined') { load(); return () => { live = false; }; }
    const observer = new IntersectionObserver((entries) => { if (entries.some((entry) => entry.isIntersecting)) { load(); observer.disconnect(); } }, { rootMargin: '120px' });
    observer.observe(ref.current);
    return () => { live = false; observer.disconnect(); };
  }, [source.id]);
  return <div ref={ref} className="group relative min-w-0">
    <button type="button" onClick={() => action.open(source)} title={source.window_title} className="recap-focus block w-full rounded-lg text-left">
      <div className="aspect-video overflow-hidden rounded-lg border border-ide-border bg-ide-active transition-colors group-hover:border-ide-accent">
        {image && <img src={image} alt="" loading="lazy" className="h-full w-full object-cover" />}
      </div>
      <span className="mt-1.5 block truncate text-[11px] text-ide-muted">{time(source.timestamp_ms)} · {source.process_name}</span>
    </button>
    <div className="absolute right-1 top-1"><AlternateSourceButton source={source} action={action} /></div>
  </div>;
}

const ROW_HEIGHT = 48;
const OVERSCAN = 8;

/** One viewport for both source dialogs and paged raw records. Unloaded rows
 * reserve their real height, so scrollbar travel never creates thousands of nodes. */
export function RecordList({ items, total = items.length, action, time, active = true, offset = 0, onOffset, onNeedMore, label }) {
  const ref = useRef(null);
  const offsetRef = useRef(offset);
  offsetRef.current = offset;
  const [viewport, setViewport] = useState({ top: offset, height: 480 });
  useLayoutEffect(() => {
    if (!active || !ref.current) return undefined;
    const node = ref.current;
    node.scrollTop = offsetRef.current;
    const measure = () => setViewport({ top: node.scrollTop, height: node.clientHeight || 480 });
    measure();
    if (typeof ResizeObserver === 'undefined') return undefined;
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    return () => observer.disconnect();
  }, [active]);
  const start = Math.max(0, Math.floor(viewport.top / ROW_HEIGHT) - OVERSCAN);
  const end = Math.min(total, Math.ceil((viewport.top + viewport.height) / ROW_HEIGHT) + OVERSCAN);
  useEffect(() => { if (active && end >= items.length && items.length < total) onNeedMore?.(); }, [active, end, items.length, total, onNeedMore]);
  return <div ref={ref} role="list" aria-label={label} tabIndex={0} className="recap-focus min-h-0 flex-1 overflow-y-auto overscroll-contain rounded-lg"
    onScroll={(event) => { const top = event.currentTarget.scrollTop; setViewport((previous) => ({ ...previous, top })); onOffset?.(top); }}>
    <div className="relative" style={{ height: total * ROW_HEIGHT }}>
      {Array.from({ length: Math.max(0, end - start) }, (_, index) => {
        const position = start + index;
        const source = items[position];
        return <div key={source?.id ?? `pending-${position}`} role="listitem" aria-setsize={total} aria-posinset={position + 1}
          className="group absolute inset-x-0 flex items-center gap-2 border-b border-ide-border/50 px-2 hover:bg-ide-hover"
          style={{ top: position * ROW_HEIGHT, height: ROW_HEIGHT }}>
          {source ? <>
            <button type="button" onClick={() => action.open(source)} title={source.window_title} className="recap-focus flex min-w-0 flex-1 items-center gap-4 rounded py-2 text-left text-xs">
              <time className="w-12 shrink-0 tabular-nums text-ide-muted">{time(source.timestamp_ms)}</time>
              <span className="min-w-0 flex-1"><span className="block truncate text-ide-text">{source.window_title || source.process_name}</span><span className="block truncate text-[11px] text-ide-muted">{source.process_name}</span></span>
            </button>
            <AlternateSourceButton source={source} action={action} />
          </> : <div aria-hidden="true" className="h-2 w-2/3 rounded bg-ide-active" />}
        </div>;
      })}
    </div>
  </div>;
}
