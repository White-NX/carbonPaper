import React, { useId, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { OverlayShell } from '../overlay/OverlayShell';
import { Dialog } from '../Dialog';
import { Button } from '../ui/Button';
import { correctRecap, recapErrorKey } from '../../lib/recap_api';
import { useDialogVisibility } from '../../hooks/useDialogFocus';
import useRecapDiagnostics from '../../hooks/useRecapDiagnostics';
import RecapProgress, { DiagnosticError } from './RecapProgress';
import { RecordList } from './RecapSources';

export function RecapError({ error }) {
  const { t } = useTranslation();
  return <p role="alert" className="text-xs leading-relaxed text-ide-error">{t(recapErrorKey(error), { defaultValue: t('recap.errors.unknown') })}</p>;
}

export function CorrectionDialog({ date, editing, threads, onSaved, onClose }) {
  const { t } = useTranslation();
  const id = useId();
  const [title, setTitle] = useState(editing.task_title);
  const [target, setTarget] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const kind = editing.kind;
  const needsTitle = kind === 'rename' || (kind === 'move' && !target);
  const submit = async (event) => {
    event.preventDefault();
    if (busy) return;
    setBusy(true); setError('');
    const correction = kind === 'rename' ? { kind, task_id: editing.task_id, title: title.trim() }
      : kind === 'merge' ? { kind, from: editing.task_id, into: target }
        : { kind, activity_id: editing.id, task_id: target || null, title: title.trim() };
    try { await correctRecap(date, correction); await onSaved(correction); }
    catch (cause) { setError(String(cause)); }
    finally { setBusy(false); }
  };
  return <OverlayShell title={t(`recap.correction.${kind}`)} subtitle={editing.task_title} onDismiss={busy ? undefined : onClose}
    footer={<><Button variant="ghost" disabled={busy} onClick={onClose}>{t('common.cancel')}</Button><Button type="submit" form={id} variant="primary" loading={busy}
      disabled={(needsTitle && !title.trim()) || (kind === 'merge' && !target)}>{t('recap.save')}</Button></>}>
    <form id={id} aria-label={t('recap.correct')} onSubmit={submit} className="space-y-4 text-sm">
      {kind !== 'rename' && <label className="block space-y-2"><span>{t('recap.correction.target')}</span>
        <select className="recap-input" value={target} disabled={busy} required={kind === 'merge'} onChange={(event) => setTarget(event.target.value)}>
          <option value="">{t(kind === 'move' ? 'recap.correction.newTask' : 'recap.correction.choose')}</option>
          {threads.filter((thread) => thread.id !== editing.task_id).map((thread) => <option key={thread.id} value={thread.id}>{thread.title}</option>)}
        </select>
      </label>}
      {needsTitle && <label className="block space-y-2"><span>{t('recap.correction.title')}</span><input className="recap-input" value={title} disabled={busy} required maxLength={120} onChange={(event) => setTitle(event.target.value)} /></label>}
      {error && <RecapError error={error} />}
    </form>
  </OverlayShell>;
}

export function SourcesDialog({ sources, action, time, onClose }) {
  const { t } = useTranslation();
  return <OverlayShell title={t('recap.sources')} subtitle={t('recap.recordCount', { count: sources.length })} size="xl" onDismiss={onClose}
    bodyClassName="flex h-[min(55vh,520px)] flex-none flex-col overflow-hidden" footer={<Button onClick={onClose}>{t('recap.close')}</Button>}>
    <RecordList items={sources} action={action} time={time} label={t('recap.sources')} />
  </OverlayShell>;
}

export function DiagnosticsDialog({ date, onClose }) {
  const { t, i18n } = useTranslation();
  const active = useDialogVisibility(true);
  const { day, progress, error } = useRecapDiagnostics(date, active);
  const displayDate = new Date(`${date}T12:00:00`).toLocaleDateString(i18n.language, { year: 'numeric', month: 'long', day: 'numeric' });
  return <Dialog isOpen onClose={onClose} maxWidth="max-w-[720px]" className="recap-diagnostics max-h-[85vh] bg-ide-panel" contentClassName="flex flex-col overflow-hidden"
    title={<><span className="block text-base font-semibold text-ide-text">{t('recap.generationDetails')}</span><span className="mt-1 block text-xs font-normal">{t('recap.title')} · {displayDate}</span></>}>
    <div className="min-h-0 overflow-y-auto px-4 py-4 sm:px-6" data-recap-diagnostics-scroll>
      {error && <div className="mb-3"><DiagnosticError error={error} /></div>}
      {!day && !error && <p role="status" className="text-sm text-ide-muted">{t('recap.loading')}</p>}
      {(day || progress) && <RecapProgress key={date} progress={progress} batches={day?.batches || []} error={day?.error} />}
    </div>
    <div className="flex shrink-0 justify-end border-t border-ide-border px-4 py-3 sm:px-6"><Button onClick={onClose}>{t('recap.close')}</Button></div>
  </Dialog>;
}
