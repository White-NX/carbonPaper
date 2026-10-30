import React, { useEffect, useId, useMemo, useRef, useState } from 'react';
import { ArrowLeft, Copy, ExternalLink, History, Info, Loader2, MoreHorizontal, Trash2, Clock, X } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { openUrl } from '@tauri-apps/plugin-opener';
import { InspectorImage } from './InspectorImage';
import DetailCard from './DetailCard';
import SnapshotSourceAction from './SnapshotSourceAction';
import { Button } from './ui/Button';
import { Banner } from './ui/Banner';
import { MenuPanel, MenuItem } from './ui/Menu';
import { ConfirmDialog } from './ConfirmDialog';
import { captureDateOf } from '../lib/search_grouping';
import { normalizeSnapshotPreviewItem, searchResultToSnapshotEvent } from '../lib/snapshot_preview';
import { setPreference, usePreference } from '../lib/preference_store';

function boxesFromDetails(details) {
  return (details?.ocr_results || []).flatMap((result, index) => {
    const points = result.box_coords || result.box;
    if (!Array.isArray(points) || !points.length) return [];
    const xs = points.map(([x]) => x), ys = points.map(([, y]) => y);
    const x = Math.min(...xs), y = Math.min(...ys);
    return [{ id: String(result.id ?? index), label: result.text, type: 'text', box: {
      x, y, width: Math.max(...xs) - x, height: Math.max(...ys) - y, unit: 'pixel',
    } }];
  });
}

/** Shared screenshot surface for records, contextual dialogs and native preview windows. */
export default function SnapshotViewer({
  item, imageSrc, details, loading = false, error, onRetry, onClose, closeLabel,
  onOpenTimeline, onOpenStandalone, onOpenRecap, onCopyText, onDeleteRecord, onDeleteNearbyRecords,
  isAuthenticated = true, active = true,
}) {
  const { t } = useTranslation();
  const panelId = useId();
  const infoVisible = usePreference('snapshotPreviewDetailsVisible', 'false') !== 'false';
  const [tab, setTab] = useState('details');
  const [copied, setCopied] = useState(false);
  const [notice, setNotice] = useState('');
  const [menu, setMenu] = useState(false);
  const [deletion, setDeletion] = useState(null);
  const [deleting, setDeleting] = useState(false);
  const copyTimer = useRef(null);
  const menuRef = useRef(null);
  const record = details?.record || {};
  const normalized = normalizeSnapshotPreviewItem(item);
  const event = searchResultToSnapshotEvent(normalized);
  const timestamp = captureDateOf(record)?.getTime() ?? event?.timestamp ?? null;
  const navigationItem = { ...normalized, captured_at_ms: timestamp };
  const selectedEvent = event ? { ...event, timestamp, category: record.category || normalized.category } : null;
  const title = record.window_title || normalized.window_title || record.process_name || normalized.process_name || t('snapshotPreview.windowTitle');
  const ocrText = (details?.ocr_results || []).map(({ text }) => text).filter(Boolean).join('\n');
  const boxes = useMemo(() => boxesFromDetails(details), [details]);

  useEffect(() => () => clearTimeout(copyTimer.current), []);
  useEffect(() => { setCopied(false); setMenu(false); setNotice(''); setDeletion(null); }, [normalized.screenshot_id, normalized.image_path]);
  useEffect(() => {
    if (!menu) return undefined;
    const pointer = (event) => { if (!menuRef.current?.contains(event.target)) setMenu(false); };
    const key = (event) => {
      if (event.key === 'Escape') { event.preventDefault(); event.stopImmediatePropagation(); setMenu(false); menuRef.current?.querySelector('button')?.focus(); }
    };
    document.addEventListener('pointerdown', pointer);
    window.addEventListener('keydown', key, true);
    return () => { document.removeEventListener('pointerdown', pointer); window.removeEventListener('keydown', key, true); };
  }, [menu]);

  const copy = async (text = ocrText) => {
    if (!text) return;
    try {
      if (onCopyText) await onCopyText(text); else await navigator.clipboard.writeText(text);
      setCopied(true); clearTimeout(copyTimer.current); copyTimer.current = setTimeout(() => setCopied(false), 1400);
    } catch { setNotice(t('snapshotPreview.copyFailed')); }
  };
  const remove = async () => {
    setDeleting(true); setNotice('');
    try {
      if (deletion === 'nearby') await onDeleteNearbyRecords(timestamp, 5);
      else await onDeleteRecord(normalized.screenshot_id);
      setDeletion(null);
    } catch (cause) { setNotice(cause?.message || String(cause)); setDeletion(null); }
    finally { setDeleting(false); }
  };
  const chooseTab = (next) => { setTab(next); };
  const hideInfo = () => {
    setPreference('snapshotPreviewDetailsVisible', false);
    document.getElementById(`${panelId}-toggle`)?.focus({ preventScroll: true });
  };
  const tabKeydown = (event) => {
    if (['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) {
      event.preventDefault();
      const next = event.key === 'Home' ? 'details' : event.key === 'End' ? 'text' : tab === 'details' ? 'text' : 'details';
      setTab(next);
      event.currentTarget.querySelector(`[data-info-tab="${next}"]`)?.focus();
    }
  };

  if (!item || !isAuthenticated) return null;
  return (
    <div className="snapshot-viewer relative isolate h-full min-h-0 w-full overflow-hidden bg-ide-bg" data-keep-selection>
      <div className="snapshot-viewer-body absolute inset-0 flex items-center justify-center overflow-hidden bg-ide-timeline-bg">
        {imageSrc ? <InspectorImage item={{ imageUrl: imageSrc, prompt: title }} overlayBoxes={boxes} onBoxClick={(box) => copy(box.label)}
          className="h-full w-full rounded-none border-0 bg-transparent" controlsPosition="bottom-right" />
          : <div role="status" className="flex flex-col items-center gap-3 p-6 text-center text-sm text-ide-muted">
            {loading && <Loader2 className="h-5 w-5 animate-spin" aria-hidden="true" />}
            {t(loading ? 'snapshotPreview.loadingImage' : 'snapshotPreview.imageUnavailable')}
          </div>}
      </div>
      <div className="snapshot-viewer-toolbar pointer-events-none absolute inset-x-3 top-3 z-20 flex items-start justify-between gap-2">
        <div className="snapshot-viewer-context pointer-events-auto flex min-w-0 items-center gap-1.5 rounded-xl border border-ide-border bg-ide-panel/95 p-1.5 shadow-lg backdrop-blur-sm">
          {onClose && <Button variant="ghost" size="xs" icon={ArrowLeft} onClick={onClose}
            title={closeLabel || t('common.close')} className="workspace-focus max-w-40 min-w-0 shrink">
            <span className="truncate">{closeLabel || t('common.close')}</span>
          </Button>}
          <div className="snapshot-viewer-title min-w-0 px-1.5">
            <p className="truncate text-xs font-medium" title={title}>{title}</p>
            {normalized.sourceLabel && normalized.sourceType !== 'main-preview' && <p className="mt-0.5 truncate text-[11px] text-ide-muted" title={normalized.sourceDetail || undefined}>{normalized.sourceLabel}{normalized.sourceDetail ? ` · ${normalized.sourceDetail}` : ''}</p>}
          </div>
        </div>
        <div className="snapshot-viewer-actions pointer-events-auto flex shrink-0 items-center gap-0.5 rounded-xl border border-ide-border bg-ide-panel/95 p-1.5 shadow-lg backdrop-blur-sm">
        <Button icon={Copy} size="xs" variant="ghost" disabled={!ocrText} onClick={() => copy()} className="workspace-focus"
          title={t('snapshotPreview.copyOcr')} aria-label={t(copied ? 'snapshotPreview.copied' : 'snapshotPreview.copyOcr')}>
          <span className="snapshot-viewer-action-label">{t(copied ? 'snapshotPreview.copied' : 'snapshotPreview.copyOcr')}</span>
        </Button>
        <SnapshotSourceAction documentRef={details?.document_ref} screenshotId={normalized.screenshot_id}
          pageUrl={record.page_url} ocrResults={details?.ocr_results} onOpenUrl={openUrl} compact placement="bottom" />
        {onOpenTimeline && <Button icon={History} size="xs" variant="ghost" onClick={() => onOpenTimeline(navigationItem)} className="workspace-focus"
          title={t('snapshotPreview.openTimeline')} aria-label={t('snapshotPreview.openTimeline')}>
          <span className="snapshot-viewer-action-label">{t('snapshotPreview.openTimeline')}</span>
        </Button>}
        {onOpenStandalone && <Button icon={ExternalLink} size="xs" variant="ghost" onClick={() => onOpenStandalone(navigationItem)} className="workspace-focus"
          title={t('snapshotPreview.openStandalone')} aria-label={t('snapshotPreview.openStandalone')}>
          <span className="snapshot-viewer-action-label">{t('snapshotPreview.openStandalone')}</span>
        </Button>}
        <Button id={`${panelId}-toggle`} icon={Info} size="xs" variant="ghost" aria-controls={panelId} aria-expanded={infoVisible}
          title={t('snapshotPreview.details')} aria-label={t('snapshotPreview.details')}
          onClick={() => setPreference('snapshotPreviewDetailsVisible', !infoVisible)} className={`workspace-focus ${infoVisible ? 'bg-ide-active text-ide-text' : ''}`}>
          <span className="snapshot-viewer-action-label">{t('snapshotPreview.details')}</span>
        </Button>
        {(onDeleteRecord || onDeleteNearbyRecords) && <div ref={menuRef} className="relative">
          <Button icon={MoreHorizontal} variant="ghost" size="xs" aria-label={t('previewAction.moreDeleteOptions')} aria-haspopup="menu" aria-expanded={menu}
            onClick={() => setMenu(!menu)} className="workspace-focus" />
          {menu && <MenuPanel role="menu" aria-label={t('previewAction.moreDeleteOptions')}>
            {onDeleteRecord && normalized.screenshot_id > 0 && <MenuItem role="menuitem" danger icon={<Trash2 className="h-3.5 w-3.5" />} onClick={() => { menuRef.current?.querySelector('button')?.focus(); setMenu(false); setDeletion('single'); }}>{t('previewAction.deleteThisRecord')}</MenuItem>}
            {onDeleteNearbyRecords && timestamp !== null && <MenuItem role="menuitem" danger icon={<Clock className="h-3.5 w-3.5" />} onClick={() => { menuRef.current?.querySelector('button')?.focus(); setMenu(false); setDeletion('nearby'); }}>{t('previewAction.deleteNearby5min')}</MenuItem>}
          </MenuPanel>}
        </div>}
        </div>
      </div>
      {(error || notice) && <Banner tone="error" className="absolute bottom-14 left-3 z-30 max-w-[calc(100%-1.5rem)]"
        action={onRetry && <Button size="xs" onClick={onRetry}>{t('common.retry')}</Button>}>{notice || error}</Banner>}
        {infoVisible && <aside id={panelId} aria-label={t('snapshotPreview.details')}
          className="snapshot-viewer-info absolute bottom-14 right-3 top-16 z-20 flex min-h-0 w-[300px] max-w-[calc(100%-1.5rem)] flex-col overflow-hidden rounded-xl border border-ide-border bg-ide-panel/95 shadow-xl backdrop-blur-sm">
          <div className="flex shrink-0 items-center gap-1 border-b border-ide-border p-1.5">
          <div role="tablist" aria-label={t('snapshotPreview.details')} onKeyDown={tabKeydown} className="flex min-w-0 flex-1 gap-1">
            {['details', 'text'].map((value) => <Button key={value} variant="ghost" size="xs" role="tab" data-info-tab={value}
              id={`${panelId}-${value}-tab`} aria-controls={`${panelId}-${value}`} aria-selected={tab === value} tabIndex={tab === value ? 0 : -1}
              onClick={() => chooseTab(value)} className={`workspace-focus flex-1 ${tab === value ? 'bg-ide-active text-ide-text' : ''}`}>
              {t(value === 'details' ? 'snapshotPreview.details' : 'snapshotPreview.ocrText')}
            </Button>)}
          </div>
            <Button variant="ghost" size="xs" icon={X} onClick={hideInfo} title={t('snapshotPreview.hideDetails')}
              aria-label={t('snapshotPreview.hideDetails')} className="workspace-focus" />
          </div>
          <div role="tabpanel" id={`${panelId}-details`} aria-labelledby={`${panelId}-details-tab`} hidden={tab !== 'details'} className="min-h-0 flex-1 overflow-y-auto">
            <DetailCard selectedEvent={selectedEvent} selectedDetails={details} active={active && tab === 'details'} isAuthenticated={isAuthenticated} onOpenRecap={onOpenRecap} docked />
          </div>
          <div role="tabpanel" id={`${panelId}-text`} aria-labelledby={`${panelId}-text-tab`} hidden={tab !== 'text'} className="min-h-0 flex-1">
            <textarea readOnly aria-label={t('snapshotPreview.ocrText')} value={ocrText}
              placeholder={t(loading ? 'snapshotPreview.loadingOcr' : record.status === 'pending' ? 'snapshotPreview.processingText' : 'snapshotPreview.noOcr')}
              className="h-full w-full resize-none bg-ide-panel p-4 text-sm leading-6 text-ide-text outline-none placeholder:text-ide-muted" />
          </div>
        </aside>}
      <ConfirmDialog isOpen={Boolean(deletion)} title={t('snapshotPreview.confirmDeleteTitle')}
        message={t(deletion === 'nearby' ? 'snapshotPreview.confirmDeleteNearby' : 'snapshotPreview.confirmDelete')}
        confirmLabel={t('previewAction.delete')} cancelLabel={t('common.cancel')} confirmVariant="danger"
        loading={deleting} loadingLabel={t('snapshotPreview.deleting')} onCancel={() => setDeletion(null)} onConfirm={remove} />
    </div>
  );
}
