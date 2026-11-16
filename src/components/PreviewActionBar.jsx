import React, { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { Trash2, ChevronUp, Clock, Maximize2, PencilLine, MousePointer2, Pencil, Eraser,
  Type, Grid2X2, Undo2, Redo2, ScanText, TextSelect } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { getSnapshotSourceOptions } from '../lib/snapshot_sources';
import SnapshotSourceAction from './SnapshotSourceAction';
import SnapshotEditorOptions from './SnapshotEditorOptions';
import { Button } from './ui/Button';
import { EDITOR_TOOL_LABELS } from '../lib/snapshot_editor';

const TOOLS = [['select', MousePointer2], ['brush', Pencil], ['erase', Eraser], ['text', Type], ['mosaic', Grid2X2]];

export default function PreviewActionBar({ selectedEvent, selectedDetails, onDeleteRecord,
  onDeleteNearbyRecords, onOpenUrl, onShowMore, onOpenFloatingPreview, showOcrPanel, editor }) {
  const { t } = useTranslation();
  const [showDeleteMenu, setShowDeleteMenu] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);
  const [peek, setPeek] = useState(true);
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  const [wrapped, setWrapped] = useState(false);
  const [lastTool, setLastTool] = useState(null);
  const barRef = useRef(null);
  const twinRef = useRef(null);
  const collapseRef = useRef(null);
  const deleteRef = useRef(null);
  const rectsRef = useRef(null);
  const editing = Boolean(editor?.editing);
  const busy = Boolean(editor?.busy || isDeleting);
  const tool = editing ? editor.selected?.kind || (editor.tool === 'select' ? null : editor.tool) : null;
  const expanded = peek || hovered || focused || editing || editor?.dirty || busy || showDeleteMenu || showOcrPanel;
  const pageUrl = selectedDetails?.record?.page_url || null;
  const documentRef = selectedDetails?.document_ref || null;
  const screenshotId = selectedDetails?.record?.id || selectedEvent?.id;
  const sourceOptions = getSnapshotSourceOptions({ documentRef, screenshotId, pageUrl, ocrResults: selectedDetails?.ocr_results });

  useEffect(() => { setPeek(true); setShowDeleteMenu(false); }, [selectedEvent?.id]);
  useEffect(() => {
    if (!peek) return undefined;
    const timer = setTimeout(() => setPeek(false), 2400);
    return () => clearTimeout(timer);
  }, [peek, selectedEvent?.id]);

  useLayoutEffect(() => {
    if (tool) setLastTool(tool);
    if (twinRef.current) twinRef.current.inert = !tool || busy;
  }, [tool, busy]);

  useLayoutEffect(() => {
    if (barRef.current) barRef.current.inert = !expanded;
    if (collapseRef.current) collapseRef.current.inert = Boolean(expanded);
  }, [expanded]);

  useLayoutEffect(() => {
    const bar = barRef.current;
    if (!bar) return undefined;
    const resize = () => { if (bar.offsetHeight) setWrapped(bar.offsetHeight > 64); };
    resize();
    const observer = new ResizeObserver(resize);
    observer.observe(bar);
    return () => observer.disconnect();
  }, []);

  // Keep existing controls in place while the capsule changes width and
  // newly revealed controls enter. Only its background stretches.
  useLayoutEffect(() => {
    const bar = barRef.current;
    if (!bar || !expanded) { rectsRef.current = null; return; }
    const controls = [...bar.querySelectorAll('[data-editor-flip]')];
    const chrome = bar.querySelector('.snapshot-editor-bar-chrome');
    const before = rectsRef.current;
    const current = { editing, chrome: chrome.getBoundingClientRect(), controls: new Map(controls.map((element) => [element, element.getBoundingClientRect()])) };
    if (before && before.editing !== editing && !window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
      for (const element of [chrome, ...controls, ...bar.querySelectorAll('[data-editor-enter]')]) element.getAnimations?.().forEach((animation) => animation.cancel());
      if (current.chrome.width && before.chrome.width) chrome.animate?.([
        { transform: 'scaleX(' + before.chrome.width / current.chrome.width + ') scaleY(' + before.chrome.height / current.chrome.height + ')' },
        { transform: 'scale(1)' },
      ], { duration: 352, easing: 'cubic-bezier(.2,.8,.2,1)' });
      for (const element of controls) {
        const old = before.controls.get(element);
        const next = current.controls.get(element);
        if (old && next.width) element.animate?.([
          { transform: 'translate(' + (old.left - next.left) + 'px,' + (old.top - next.top) + 'px)' }, { transform: 'translate(0,0)' },
        ], { duration: 320, easing: 'cubic-bezier(.2,.8,.2,1)' });
      }
      if (editing) bar.querySelectorAll('[data-editor-enter]').forEach((element) => element.animate?.([
        { opacity: 0, transform: 'translateY(7px)' }, { opacity: 1, transform: 'translateY(0)' },
      ], { duration: 208, delay: 176, fill: 'backwards', easing: 'ease-out' }));
    }
    rectsRef.current = current;
  });

  useEffect(() => {
    if (!showDeleteMenu) return undefined;
    const close = (event) => { if (!deleteRef.current?.contains(event.target)) setShowDeleteMenu(false); };
    document.addEventListener('pointerdown', close);
    return () => document.removeEventListener('pointerdown', close);
  }, [showDeleteMenu]);

  const deleteRecord = useCallback(async () => {
    if (!selectedEvent?.id) return;
    setIsDeleting(true);
    try { await onDeleteRecord?.(selectedEvent.id); }
    finally { setIsDeleting(false); setShowDeleteMenu(false); }
  }, [selectedEvent?.id, onDeleteRecord]);
  const deleteNearby = useCallback(async () => {
    if (selectedEvent?.timestamp == null) return;
    setIsDeleting(true);
    try { await onDeleteNearbyRecords?.(selectedEvent.timestamp, 5); }
    finally { setIsDeleting(false); setShowDeleteMenu(false); }
  }, [selectedEvent?.timestamp, onDeleteNearbyRecords]);
  const guarded = (action) => {
    setShowDeleteMenu(false);
    if (editor?.dirty) editor.requestLeave(action);
    else void action?.();
  };

  if (!selectedEvent) return null;
  const buttonClass = 'snapshot-editor-action border-0 text-ide-text';

  return (
    <div className="snapshot-editor-dock" data-expanded={Boolean(expanded)} data-editing={editing} data-keep-selection
      onMouseEnter={() => setHovered(true)} onMouseLeave={() => setHovered(false)}
      onFocusCapture={(event) => setFocused(!collapseRef.current?.contains(event.target))}
      onBlurCapture={(event) => { if (!event.currentTarget.contains(event.relatedTarget)) setFocused(false); }}>
      <div className="snapshot-editor-twin-space">
        <div ref={twinRef} className="snapshot-editor-twin preview-action-bar" data-open={Boolean(tool)} aria-hidden={!tool}>
          {editor && <SnapshotEditorOptions key={tool || lastTool} editor={editor} type={tool || lastTool} />}
        </div>
      </div>
      <div ref={barRef} className="snapshot-editor-mainbar" data-wrapped={wrapped} role="toolbar" aria-label={t('snapshotEditor.actions')} aria-hidden={!expanded}>
        <div className="snapshot-editor-bar-chrome preview-action-bar" aria-hidden="true" />
        <div className="snapshot-editor-legacy" role="group" aria-label={t('snapshotEditor.snapshotActions')}>
          <Button data-editor-flip icon={PencilLine} variant="ghost" size="xs" className={buttonClass}
            disabled={!editor?.canEdit || busy} aria-pressed={editing} title={t(editing ? 'snapshotEditor.exit' : 'snapshotEditor.edit')}
            onClick={editor?.toggleEditing}>{t('snapshotEditor.edit')}</Button>
          {sourceOptions.length > 0 && (
            <div data-editor-flip><SnapshotSourceAction documentRef={documentRef} screenshotId={screenshotId}
              pageUrl={pageUrl} ocrResults={selectedDetails?.ocr_results} onOpenUrl={onOpenUrl} compact /></div>
          )}
          <Button data-editor-flip icon={Maximize2} variant="ghost" size="xs" className={buttonClass} disabled={busy}
            onClick={() => guarded(onOpenFloatingPreview)} title={t('previewAction.openFloatingPreview')}>{t('previewAction.float')}</Button>
          <Button data-editor-flip icon={TextSelect} variant="ghost" size="xs" className={buttonClass} disabled={busy}
            onClick={onShowMore} aria-pressed={showOcrPanel} title={t('previewAction.showOcr')}>{t('snapshotEditor.recognizedText')}</Button>
        </div>
        <div className="snapshot-editor-tools" data-editor-enter role="group" aria-label={t('snapshotEditor.toolsLabel')} hidden={!editing}>
          <span className="snapshot-editor-divider" aria-hidden="true" />
          {TOOLS.map(([name, Icon]) => (
            <Button key={name} variant="ghost" size="xs" icon={Icon} className="snapshot-editor-action snapshot-editor-icon border-0"
              disabled={busy} aria-pressed={editor?.tool === name} aria-label={t(EDITOR_TOOL_LABELS[name])}
              title={t(EDITOR_TOOL_LABELS[name])} onClick={() => editor.chooseTool(name)} />
          ))}
        </div>
        <div className="snapshot-editor-session" data-editor-enter role="group" aria-label={t('snapshotEditor.history')} hidden={!editing}>
          <span className="snapshot-editor-divider" aria-hidden="true" />
          <Button variant="ghost" size="xs" icon={Undo2} className="snapshot-editor-action snapshot-editor-icon border-0"
            disabled={busy || !editor?.undo.length} aria-label={t('snapshotEditor.undo')} title={t('snapshotEditor.undo')}
            onClick={editor?.undoAction} />
          <Button variant="ghost" size="xs" icon={Redo2} className="snapshot-editor-action snapshot-editor-icon border-0"
            disabled={busy || !editor?.redo.length} aria-label={t('snapshotEditor.redo')} title={t('snapshotEditor.redo')}
            onClick={editor?.redoAction} />
          <Button variant="primary" size="xs" icon={ScanText} className="snapshot-editor-action snapshot-editor-save"
            disabled={!editor?.dirty} loading={editor?.busy} onClick={() => editor.save()}>{t(editor?.busy ? 'snapshotEditor.saving' : 'snapshotEditor.save')}</Button>
        </div>
        <div ref={deleteRef} data-editor-flip className="snapshot-editor-delete">
          <Button icon={Trash2} variant="ghost" size="xs" className={buttonClass} onClick={() => guarded(deleteRecord)}
            disabled={busy} title={t('previewAction.deleteNowTitle')}>{t('previewAction.delete')}</Button>
          <Button variant="ghost" size="xs" className="snapshot-editor-delete-arrow border-0" disabled={busy}
            onClick={() => setShowDeleteMenu((previous) => !previous)} aria-label={t('previewAction.moreDeleteOptions')}
            aria-haspopup="menu" aria-expanded={showDeleteMenu} title={t('previewAction.moreDeleteOptions')}>
            <ChevronUp size={12} className={showDeleteMenu ? 'rotate-180' : ''} aria-hidden="true" />
          </Button>
          {showDeleteMenu && (
            <div className="snapshot-editor-delete-menu" role="menu" aria-label={t('previewAction.moreDeleteOptions')}
              onKeyDown={(event) => { if (event.key === 'Escape') { event.stopPropagation(); setShowDeleteMenu(false); deleteRef.current.querySelector('[aria-haspopup]').focus(); } }}>
              <Button role="menuitem" icon={Trash2} variant="ghost" size="xs" disabled={busy}
                onClick={() => guarded(deleteRecord)}>{t('previewAction.deleteThisRecord')}</Button>
              <Button role="menuitem" icon={Clock} variant="ghost" size="xs" className="text-ide-error hover:text-ide-error" disabled={busy}
                onClick={() => guarded(deleteNearby)}>{t('previewAction.deleteNearby5min')}</Button>
            </div>
          )}
        </div>
      </div>
      <div ref={collapseRef} className="snapshot-editor-collapse" aria-hidden={Boolean(expanded)}>
        <Button variant="secondary" size="xs" className="snapshot-editor-collapse-button preview-action-bar"
          aria-label={t('snapshotEditor.expandActions')} title={t('snapshotEditor.expandActions')}
          onClick={() => {
            setPeek(true);
            requestAnimationFrame(() => barRef.current?.querySelector('button:not(:disabled)')?.focus({ preventScroll: true }));
          }}>
          <ChevronUp size={20} aria-hidden="true" />
        </Button>
      </div>
    </div>
  );
}
