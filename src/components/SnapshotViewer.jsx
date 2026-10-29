import React, { useEffect, useId, useMemo, useRef, useState } from 'react';
import { ArrowLeft, Copy, ExternalLink, History, Info, Loader2, MoreHorizontal, Trash2, Clock } from 'lucide-react';
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
  const infoVisible = usePreference('snapshotPreviewDetailsVisible', 'true') !== 'false';
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
    <div className="snapshot-viewer flex h-full min-h-0 w-full flex-col overflow-hidden bg-ide-bg" data-keep-selection>
      <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-ide-border bg-ide-panel px-3 py-2">
        {onClose && <Button variant="ghost" size="xs" icon={ArrowLeft} onClick={onClose} className="workspace-focus">{closeLabel || t('common.close')}</Button>}
        <div className="min-w-0 flex-1 basis-32">
          <p className="truncate text-xs font-medium" title={title}>{title}</p>
          {normalized.sourceLabel && <p className="mt-0.5 truncate text-[11px] text-ide-muted" title={normalized.sourceDetail || undefined}>{normalized.sourceLabel}{normalized.sourceDetail ? ` · ${normalized.sourceDetail}` : ''}</p>}
        </div>
        <Button icon={Copy} size="xs" variant="ghost" disabled={!ocrText} onClick={() => copy()} className="workspace-focus"
          title={t('snapshotPreview.copyOcr')}>{t(copied ? 'snapshotPreview.copied' : 'snapshotPreview.copyOcr')}</Button>
        <SnapshotSourceAction documentRef={details?.document_ref} screenshotId={normalized.screenshot_id}
          pageUrl={record.page_url} ocrResults={details?.ocr_results} onOpenUrl={openUrl} compact placement="bottom" />
        {onOpenTimeline && <Button icon={History} size="xs" variant="ghost" onClick={() => onOpenTimeline(navigationItem)} className="workspace-focus">{t('snapshotPreview.openTimeline')}</Button>}
        {onOpenStandalone && <Button icon={ExternalLink} size="xs" variant="ghost" onClick={() => onOpenStandalone(navigationItem)} className="workspace-focus">{t('snapshotPreview.openStandalone')}</Button>}
        <Button icon={Info} size="xs" variant="ghost" aria-controls={panelId} aria-expanded={infoVisible}
          onClick={() => setPreference('snapshotPreviewDetailsVisible', !infoVisible)} className={`workspace-focus ${infoVisible ? 'bg-ide-active text-ide-text' : ''}`}>{t('snapshotPreview.details')}</Button>
        {(onDeleteRecord || onDeleteNearbyRecords) && <div ref={menuRef} className="relative">
          <Button icon={MoreHorizontal} variant="ghost" size="xs" aria-label={t('previewAction.moreDeleteOptions')} aria-haspopup="menu" aria-expanded={menu}
            onClick={() => setMenu(!menu)} className="workspace-focus" />
          {menu && <MenuPanel role="menu" aria-label={t('previewAction.moreDeleteOptions')}>
            {onDeleteRecord && normalized.screenshot_id > 0 && <MenuItem role="menuitem" danger icon={<Trash2 className="h-3.5 w-3.5" />} onClick={() => { menuRef.current?.querySelector('button')?.focus(); setMenu(false); setDeletion('single'); }}>{t('previewAction.deleteThisRecord')}</MenuItem>}
            {onDeleteNearbyRecords && timestamp !== null && <MenuItem role="menuitem" danger icon={<Clock className="h-3.5 w-3.5" />} onClick={() => { menuRef.current?.querySelector('button')?.focus(); setMenu(false); setDeletion('nearby'); }}>{t('previewAction.deleteNearby5min')}</MenuItem>}
          </MenuPanel>}
        </div>}
      </div>
      {(error || notice) && <Banner tone="error" className="m-2 shrink-0" action={onRetry && <Button size="xs" onClick={onRetry}>{t('common.retry')}</Button>}>{notice || error}</Banner>}
      <div className={`snapshot-viewer-body min-h-0 flex-1 ${infoVisible ? 'snapshot-viewer-body--details' : ''}`}>
        <div className="relative flex min-h-0 min-w-0 items-center justify-center overflow-hidden bg-ide-timeline-bg">
          {imageSrc ? <InspectorImage item={{ imageUrl: imageSrc, prompt: title }} overlayBoxes={boxes} onBoxClick={(box) => copy(box.label)}
            className="h-full w-full rounded-none border-0 bg-transparent" />
            : <div role="status" className="flex flex-col items-center gap-3 p-6 text-center text-sm text-ide-muted">
              {loading && <Loader2 className="h-5 w-5 animate-spin" aria-hidden="true" />}
              {t(loading ? 'snapshotPreview.loadingImage' : 'snapshotPreview.imageUnavailable')}
            </div>}
        </div>
        {infoVisible && <aside id={panelId} className="snapshot-viewer-info flex min-h-0 flex-col overflow-hidden border-l border-ide-border bg-ide-panel">
          <div role="tablist" aria-label={t('snapshotPreview.details')} onKeyDown={tabKeydown} className="flex shrink-0 gap-1 border-b border-ide-border p-2">
            {['details', 'text'].map((value) => <Button key={value} variant="ghost" size="xs" role="tab" data-info-tab={value}
              id={`${panelId}-${value}-tab`} aria-controls={`${panelId}-${value}`} aria-selected={tab === value} tabIndex={tab === value ? 0 : -1}
              onClick={() => chooseTab(value)} className={`workspace-focus flex-1 ${tab === value ? 'bg-ide-active text-ide-text' : ''}`}>
              {t(value === 'details' ? 'snapshotPreview.details' : 'snapshotPreview.ocrText')}
            </Button>)}
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
      </div>
      <ConfirmDialog isOpen={Boolean(deletion)} title={t('snapshotPreview.confirmDeleteTitle')}
        message={t(deletion === 'nearby' ? 'snapshotPreview.confirmDeleteNearby' : 'snapshotPreview.confirmDelete')}
        confirmLabel={t('previewAction.delete')} cancelLabel={t('common.cancel')} confirmVariant="danger"
        loading={deleting} loadingLabel={t('snapshotPreview.deleting')} onCancel={() => setDeletion(null)} onConfirm={remove} />
    </div>
  );
}
