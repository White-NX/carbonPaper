import React, { forwardRef, useImperativeHandle, useLayoutEffect, useRef } from 'react';
import { createPortal } from 'react-dom';
import { ImageOff, Loader2, Monitor } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import useTimelineHoverPreview from '../../hooks/useTimelineHoverPreview';
import { timelinePreviewPosition } from '../../lib/timeline_interaction';
import { getProcessColor } from '../../lib/timeline_palette';

const TimelineHoverPreview = forwardRef(function TimelineHoverPreview({ enabled, revision, onBusyChange }, ref) {
  const { t, i18n } = useTranslation();
  const preview = useTimelineHoverPreview({ enabled, revision, onBusyChange });
  const cardRef = useRef(null);
  const positionRef = useRef(null);
  const position = () => {
    if (!cardRef.current || !positionRef.current) return;
    const next = timelinePreviewPosition(positionRef.current, window.innerWidth, window.innerHeight);
    Object.assign(cardRef.current.style, { left: `${next.left}px`, top: `${next.top}px`, width: `${next.width}px` });
    cardRef.current.style.setProperty('--tl-preview-image-height', `${next.imageHeight}px`);
  };
  useImperativeHandle(ref, () => ({
    show: (target) => { positionRef.current = target; position(); preview.show(target); },
    hide: preview.hide,
    read: preview.read,
  }));
  useLayoutEffect(position);

  const { state } = preview;
  if (!enabled || (!state.event && !state.visible)) return null;
  const icon = state.event?.processIcon;
  return createPortal(
    <div
      ref={cardRef}
      className="tl-hover-preview"
      data-visible={state.visible}
      role="tooltip"
      aria-hidden={!state.visible}
      aria-label={t('timeline.snapshotPreview')}
    >
      <div className="tl-hover-preview-image">
        {state.imageSrc ? <img key={state.event?.id} src={state.imageSrc} alt={t('timeline.snapshotPreview')} draggable={false} onError={() => preview.reportImageError(state.event?.id)} />
          : <span className="flex h-full items-center justify-center text-ide-muted">
            {state.error ? <ImageOff size={28} /> : <Loader2 size={24} className="animate-spin" />}
          </span>}
      </div>
      <div className="flex items-center gap-2 px-1 pt-2">
        {icon ? <img src={icon.startsWith('data:') ? icon : `data:image/png;base64,${icon}`} alt="" className="h-4 w-4 shrink-0 rounded-sm" />
          : <Monitor size={15} className="shrink-0" style={{ color: getProcessColor(state.event?.appName) }} />}
        <span className="min-w-0 flex-1 truncate text-xs font-medium text-ide-text">{state.event?.appName || t('timeline.snapshotPreview')}</span>
        <time className="shrink-0 font-mono text-[11px] text-ide-accent">
          {state.event && new Date(state.event.timestamp).toLocaleTimeString(i18n.language, { hour12: false })}
        </time>
      </div>
      <div className="truncate px-1 pt-1 text-[11px] text-ide-muted">
        {state.error ? t('timeline.previewUnavailable') : state.event?.windowTitle || '\u00a0'}
      </div>
    </div>, document.body,
  );
});

export default TimelineHoverPreview;
