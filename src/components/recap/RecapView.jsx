import React, { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ArrowLeft, CalendarDays, ChevronDown, ChevronLeft, ChevronRight, FileText, Merge, MoreHorizontal, Pencil, RefreshCw, Trash2, Undo2 } from 'lucide-react';
import { grantAiRemoteConsent } from '../../lib/ai_api';
import { cancelRecap, correctRecap, deleteRecap, generateRecap, localDate } from '../../lib/recap_api';
import { ConfirmDialog } from '../ConfirmDialog';
import { openSettingsWindow } from '../../lib/settings_api';
import useRecapDay from '../../hooks/useRecapDay';
import { Button } from '../ui/Button';
import { MenuItem, MenuPanel } from '../ui/Menu';
import { CorrectionDialog, DiagnosticsDialog, RecapError, SourcesDialog } from './RecapDialogs';
import { SourceThumbnail, useRecapSourceAction } from './RecapSources';
import { PageHeader } from '../PageHeader';
import { RecapTabs } from './RecapTabs';
import RecapRecords from './RecapRecords';
import RecapRunStatus from './RecapRunStatus';
import RecapApps from './RecapApps';

function ActionMenu({ label, icon: Icon = MoreHorizontal, items, text = false }) {
  const [open, setOpen] = useState(false);
  const ref = useRef(null);
  useEffect(() => {
    if (!open) return undefined;
    ref.current?.querySelector('[role="menuitem"]:not(:disabled)')?.focus();
    const outside = (event) => { if (!ref.current?.contains(event.target)) setOpen(false); };
    document.addEventListener('pointerdown', outside);
    return () => document.removeEventListener('pointerdown', outside);
  }, [open]);
  return <div ref={ref} className="relative" onBlur={(event) => { if (!event.currentTarget.contains(event.relatedTarget)) setOpen(false); }}
    onKeyDown={(event) => {
      if (event.key === 'Escape' && open) { event.stopPropagation(); setOpen(false); ref.current?.querySelector('button')?.focus(); }
      if (open && ['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
        event.preventDefault();
        const buttons = [...ref.current.querySelectorAll('[role="menuitem"]:not(:disabled)')];
        const index = buttons.indexOf(document.activeElement);
        const next = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1 : (index + (event.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length;
        buttons[next]?.focus();
      }
    }}>
    {text ? <Button variant="ghost" icon={Icon} aria-label={label} aria-haspopup="menu" aria-expanded={open} onClick={() => setOpen(!open)}>{label}</Button>
      : <button type="button" aria-label={label} title={label} aria-haspopup="menu" aria-expanded={open} onClick={() => setOpen(!open)}
        className="recap-focus grid h-6 w-6 place-items-center rounded text-ide-muted transition-opacity hover:bg-ide-hover hover:text-ide-text">
        <Icon className="h-3.5 w-3.5" aria-hidden="true" />
      </button>}
    {open && <MenuPanel role="menu" aria-label={label}>
      {items.map(({ icon: ItemIcon, ...item }) => <MenuItem key={item.label} role="menuitem" disabled={item.disabled} danger={item.danger}
        icon={ItemIcon && <ItemIcon className="h-3.5 w-3.5 shrink-0" aria-hidden="true" />} className="recap-focus"
        onClick={() => { ref.current?.querySelector('button')?.focus(); setOpen(false); item.onClick(); }}>{item.label}</MenuItem>)}
    </MenuPanel>}
  </div>;
}

function Period({ batch, time, onTask, onDetails }) {
  const { t } = useTranslation();
  const topics = batch.summary?.topics?.length ? batch.summary.topics : [...new Map(batch.activities.map((activity) => [activity.task_id, activity])).values()].map((activity) => ({ task_id: activity.task_id, title: activity.task_title, text: activity.text }));
  const problem = batch.error || batch.summary_error;
  return <article className="recap-period">
    <div className="recap-period-time"><span className="recap-time-dot" /><h2 className="text-xs font-medium tabular-nums text-ide-muted">{time(batch.start_ms)}–{time(batch.end_ms)}</h2></div>
    <div className="min-w-0 pb-9">
      {batch.summary?.overview && <p className={`${batch.apps?.length ? 'mb-2' : 'mb-5'} whitespace-pre-wrap text-sm leading-7`}>{batch.summary.overview}</p>}
      <RecapApps apps={batch.apps} />
      {problem ? <div className="mb-3 flex flex-wrap items-center gap-3"><RecapError error={problem} /><button className="recap-focus rounded text-xs text-ide-muted hover:text-ide-text" onClick={onDetails}>{t('recap.generationDetails')}</button></div>
        : !batch.summary && topics.length > 0 && <p className="mb-3 text-xs text-ide-muted">{t('recap.overviewPending')}</p>}
      {topics.length > 0 ? <div className="divide-y divide-ide-border/60 border-y border-ide-border/60">
        {topics.map((topic) => <button key={topic.task_id} data-recap-task={topic.task_id} data-recap-entry={`${batch.start_ms}:${topic.task_id}`} onClick={() => onTask(topic.task_id, batch.start_ms)}
          className="recap-focus group flex w-full items-center gap-5 px-3 py-4 text-left transition-colors hover:bg-ide-hover">
          <span className="min-w-0 flex-1"><span className="block text-sm font-medium">{topic.title}</span><span className="mt-1.5 line-clamp-2 text-[13px] leading-relaxed text-ide-muted">{topic.text}</span></span>
          <ChevronRight className="h-4 w-4 shrink-0 text-ide-muted transition-transform group-hover:translate-x-0.5" aria-hidden="true" />
        </button>)}
      </div> : !problem && <p className="text-sm text-ide-muted">{t(batch.status === 'pending' ? 'recap.waitingPeriod' : 'recap.noActivities')}</p>}
    </div>
  </article>;
}

function Landing({ enabled, hasClosedPeriod, loading, error, onConfigure, onGenerate, onRetry, running }) {
  const { t } = useTranslation();
  if (loading) return <div aria-label={t('recap.loading')} role="status" className="space-y-10 py-8">
    {[0, 1, 2].map((key) => <div key={key} className="recap-period motion-safe:animate-pulse"><div className="h-3 w-20 rounded bg-ide-active" /><div className="space-y-4"><div className="h-3 w-4/5 rounded bg-ide-active" /><div className="h-3 w-3/5 rounded bg-ide-active" /><div className="h-3 w-2/3 rounded bg-ide-active" /></div></div>)}
  </div>;
  return <div className="mx-auto max-w-xl py-12 sm:py-20">
    <div className="mb-6 flex h-12 w-12 items-center justify-center rounded-xl border border-ide-border bg-ide-panel text-ide-accent"><CalendarDays className="h-6 w-6" /></div>
    <h2 className="text-xl font-semibold tracking-tight">{t(!enabled ? 'recap.welcome' : 'recap.emptyTitle')}</h2>
    <p className="mt-3 max-w-lg text-sm leading-7 text-ide-muted">{t(!enabled ? 'recap.intro' : hasClosedPeriod ? 'recap.emptyDescription' : 'recap.waitingDescription')}</p>
    <div className="mt-6">
      {error ? <Button onClick={onRetry}>{t('common.retry')}</Button> : !enabled ? <Button variant="primary" onClick={onConfigure}>{t('recap.configure')}</Button>
        : hasClosedPeriod && !running && <Button variant="primary" icon={RefreshCw} onClick={onGenerate}>{t('recap.generateFirst')}</Button>}
    </div>
  </div>;
}

export default function RecapView({ active, isAuthenticated, onSelectScreenshot, onOpenSnapshotPreview }) {
  const { t, i18n } = useTranslation();
  const [date, setDate] = useState(localDate);
  const [task, setTask] = useState(null);
  const [tab, setTab] = useState('recap');
  const [period, setPeriod] = useState('');
  const [editing, setEditing] = useState(null);
  const [sources, setSources] = useState(null);
  const [diagnostics, setDiagnostics] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [operation, setOperation] = useState('');
  const [error, setError] = useState('');
  const [liveRunning, setLiveRunning] = useState(false);
  const [notice, setNotice] = useState('');
  const { day, settings, days, error: loadError, refresh } = useRecapDay(date, active, isAuthenticated);
  const action = useRecapSourceAction(onSelectScreenshot, onOpenSnapshotPreview);
  const scroll = useRef(null);
  const positions = useRef({});
  const recordsPosition = useRef(0);
  const focusTask = useRef(null);
  const originEntry = useRef(null);
  const epoch = useRef(0);
  const formatter = useMemo(() => new Intl.DateTimeFormat(i18n.language, { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' }), [i18n.language]);
  const time = useCallback((ms) => formatter.format(new Date(ms)), [formatter]);
  const route = task ? `task:${task}` : tab;
  const batches = day?.batches || [];
  const activities = useMemo(() => (day?.batches || []).flatMap((batch) => batch.activities).filter((activity) => activity.task_id === task).sort((a, b) => a.start_ms - b.start_ms), [day, task]);
  const selected = day?.threads.find((thread) => thread.id === task);
  const loaded = Boolean(day);
  const hasContent = batches.some((batch) => batch.activities.length || batch.error || batch.summary || batch.summary_error);
  const running = operation === 'generate' || liveRunning || Boolean(day?.running);
  const issue = error || loadError || day?.error;
  const consentNeeded = String(issue).includes('AI_REMOTE_CONSENT_REQUIRED') || batches.some((batch) => [batch.error, batch.summary_error].some((value) => value?.includes('AI_REMOTE_CONSENT_REQUIRED')));
  const latest = Math.max(0, ...batches.map((batch) => batch.updated_at_ms || 0));
  const back = useCallback(() => { focusTask.current = { task, entry: originEntry.current }; setTask(null); }, [task]);
  const enterTask = (id, start) => { originEntry.current = `${start}:${id}`; setTask(id); };

  useEffect(() => {
    epoch.current += 1;
    setConfirmDelete(false);
    setTask(null); setEditing(null); setSources(null); setDiagnostics(false); setOperation(''); setError(''); setNotice(''); setLiveRunning(false);
    setTab('recap'); setPeriod(''); positions.current = {}; recordsPosition.current = 0;
  }, [date, isAuthenticated]);
  useEffect(() => {
    if (!active) return undefined;
    const escape = (event) => {
      if (event.key === 'Escape' && task && !editing && !sources && !diagnostics && !event.defaultPrevented) { event.preventDefault(); back(); }
    };
    window.addEventListener('keydown', escape);
    return () => window.removeEventListener('keydown', escape);
  }, [active, task, editing, sources, diagnostics, back]);
  useLayoutEffect(() => {
    if (!active || !scroll.current) return;
    scroll.current.scrollTop = positions.current[route] || 0;
    if (!task && focusTask.current) {
      const entries = [...scroll.current.querySelectorAll('[data-recap-task]')];
      const entry = entries.find((node) => node.dataset.recapEntry === focusTask.current.entry) || entries.find((node) => node.dataset.recapTask === focusTask.current.task);
      entry?.focus({ preventScroll: true });
      focusTask.current = null;
    }
  }, [active, route, task, loaded]);

  const run = async (kind, work) => {
    if (operation) return;
    const token = epoch.current;
    setOperation(kind); setError('');
    try { await work(); if (token === epoch.current) await refresh(); }
    catch (cause) { if (token === epoch.current) setError(String(cause)); }
    finally { if (token === epoch.current) setOperation(''); }
  };
  const generate = (force = false) => run('generate', () => generateRecap(date, force));
  const remove = () => {
    if (running || operation) return;
    const token = epoch.current;
    return run('delete', async () => {
      try {
        await deleteRecap(date);
        if (token === epoch.current) {
          setTask(null); setTab('recap'); setPeriod(''); setNotice('');
          positions.current = {}; recordsPosition.current = 0;
        }
      } finally {
        if (token === epoch.current) setConfirmDelete(false);
      }
    });
  };
  const configure = () => openSettingsWindow('organize', 'daily-recap').catch((cause) => setError(String(cause)));
  const shift = (delta) => { const value = new Date(`${date}T12:00:00`); value.setDate(value.getDate() + delta); setDate(localDate(value)); };
  const editTask = (kind) => setEditing({ kind, task_id: task, task_title: selected?.title || activities[0]?.task_title || '', epoch: epoch.current });
  const correctionSaved = async (correction, token) => {
    if (token !== epoch.current) return;
    setEditing(null); setNotice(t('recap.corrected'));
    if (correction.kind === 'merge') setTask(correction.into);
    await refresh();
  };
  const undo = () => run('undo', async () => { await correctRecap(date, { kind: 'undo' }); setNotice(''); });

  if (!active) return null;
  if (!isAuthenticated) return <div className="m-auto p-8 text-sm text-ide-muted">{t('recap.locked')}</div>;
  return <div className="recap-surface relative flex min-h-0 flex-1 flex-col bg-ide-bg text-ide-text">
    <PageHeader
      as="header"
      bordered
      flushBottom={Boolean(!task && settings?.enabled)}
      secondaryRow={!task && settings?.enabled ? (
        <>
          <RecapTabs tab={tab} onChange={setTab} />
          {latest > 0 && (
            <span className="ml-auto hidden text-[11px] text-ide-muted sm:block">
              {t('recap.updated', { time: time(latest) })}
            </span>
          )}
        </>
      ) : undefined}
    >
      <div className="recap-header grid min-w-0 flex-1 items-start gap-x-5 gap-y-2">
        <div className="flex h-10 min-w-0 items-center">
          {task ? <Button variant="ghost" icon={ArrowLeft} onClick={back}>{t('recap.back')}</Button>
            : <h1 className="flex min-w-0 items-center gap-2 text-sm font-semibold text-ide-text"><CalendarDays className="h-4 w-4 shrink-0 text-ide-accent" /><span className="truncate">{t('recap.title')}</span></h1>}
        </div>
        <div className="recap-header-actions flex min-h-10 items-center justify-end gap-1.5">
          {date !== localDate() && <Button variant="ghost" onClick={() => setDate(localDate())}>{t('recap.today')}</Button>}
          {task ? <ActionMenu label={t('recap.editEvent')} icon={ChevronDown} text items={[
            { label: t('recap.correction.rename'), icon: Pencil, onClick: () => editTask('rename'), disabled: !selected },
            { label: t('recap.correction.merge'), icon: Merge, onClick: () => editTask('merge'), disabled: !selected || day.threads.length < 2 },
          ]} /> : (settings?.enabled || hasContent || days.includes(date)) && <>
            {running ? <Button onClick={() => cancelRecap().catch((cause) => setError(String(cause)))}>{t('recap.stop')}</Button>
              : hasContent && settings?.enabled && <Button icon={RefreshCw} disabled={Boolean(operation) || !batches.length} onClick={() => generate()}>{t('recap.generate')}</Button>}
            <ActionMenu label={t('recap.more')} items={[
              { label: t('recap.regenerate'), icon: RefreshCw, onClick: () => generate(true), disabled: !settings?.enabled || running || Boolean(operation) || !batches.length },
              { label: t('recap.generationDetails'), icon: FileText, onClick: () => setDiagnostics(true) },
              { label: t('recap.undo'), icon: Undo2, onClick: undo, disabled: !day?.can_undo || Boolean(operation) },
              { label: t('recap.delete'), icon: Trash2, danger: true, onClick: () => setConfirmDelete(true), disabled: running || Boolean(operation) || !day || !(hasContent || days.includes(date)) },
            ]} />
          </>}
        </div>
        <div role="group" aria-label={t('recap.date')} className="recap-date-navigation flex h-10 shrink-0 items-center gap-1.5">
          <Button variant="ghost" icon={ChevronLeft} aria-label={t('recap.previous')} onClick={() => shift(-1)} />
          <input type="date" aria-label={t('recap.date')} value={date} max={localDate()} list="recap-dates" onChange={(event) => { if (event.target.value && event.target.value <= localDate()) setDate(event.target.value); }}
            className="recap-focus h-[30px] w-36 min-w-0 rounded-lg border border-ide-border bg-ide-bg px-2.5 text-xs tabular-nums text-ide-text" />
          <datalist id="recap-dates">{days.map((value) => <option key={value} value={value} />)}</datalist>
          <Button variant="ghost" icon={ChevronRight} aria-label={t('recap.next')} disabled={date >= localDate()} onClick={() => shift(1)} />
        </div>
      </div>
    </PageHeader>
    <RecapRunStatus key={date} date={date} active={active} onRunning={setLiveRunning} onComplete={refresh} />
    {issue && <div className="flex shrink-0 flex-wrap items-center gap-3 border-b border-ide-border px-6 py-3"><RecapError error={issue} /><Button variant="ghost" onClick={refresh}>{t('common.retry')}</Button></div>}
    {consentNeeded && <div role="alert" className="flex shrink-0 flex-wrap items-center gap-3 border-b border-ide-border px-6 py-3 text-xs"><p className="flex-1 leading-relaxed">{t('recap.remoteConsent')}</p><Button disabled={Boolean(operation)} onClick={() => run('generate', async () => { await grantAiRemoteConsent(); await generateRecap(date); })}>{t('recap.allowRemote')}</Button></div>}
    {notice && <div role="status" className="flex shrink-0 items-center gap-3 border-b border-ide-border px-6 py-2 text-xs text-ide-muted">{notice}<Button variant="ghost" icon={Undo2} onClick={undo} disabled={!day?.can_undo || Boolean(operation)}>{t('recap.undo')}</Button></div>}
    {!task && tab === 'records' && settings?.enabled ? <div id="recap-panel-records" role="tabpanel" aria-labelledby="recap-tab-records" className="min-h-0 flex-1">
      <RecapRecords date={date} batches={batches} active={active} action={action} time={time} period={period} onPeriod={setPeriod} position={recordsPosition} />
    </div> : <div ref={scroll} id="recap-panel-recap" role={!task ? 'tabpanel' : undefined} aria-labelledby={!task && settings?.enabled ? 'recap-tab-recap' : undefined}
      className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-6 py-7" onScroll={(event) => { positions.current[route] = event.currentTarget.scrollTop; }}>
      <div className="mx-auto max-w-[1120px]">
        {task ? <div className="mx-auto max-w-[880px]">
          <div className="mb-8"><p className="mb-2 text-xs text-ide-muted">{date} · {t('recap.eventHistory')}</p><h1 className="break-words text-xl font-semibold leading-relaxed">{selected?.title || activities[0]?.task_title || t('recap.eventUnavailable')}</h1></div>
          {activities.length === 0 && <p className="text-sm text-ide-muted">{t('recap.eventEmpty')}</p>}
          {activities.map((activity) => <article key={activity.id} className="border-t border-ide-border/70 py-6">
            <div className="mb-3 flex items-center justify-between gap-3"><time className="text-xs font-medium tabular-nums text-ide-muted">{time(activity.start_ms)}{activity.start_ms !== activity.end_ms && `–${time(activity.end_ms)}`}</time>
              <Button variant="ghost" icon={Pencil} onClick={() => setEditing({ ...activity, kind: 'move', epoch: epoch.current })}>{t('recap.adjust')}</Button></div>
            <p className="whitespace-pre-wrap break-words text-sm leading-7">{activity.text}</p>
            {activity.sources.length > 0 && <div className="mt-4"><div className="grid max-w-lg grid-cols-3 gap-3">{activity.sources.slice(0, 3).map((source) => <SourceThumbnail key={source.id} source={source} action={action} time={time} />)}</div>
              <button className="recap-focus mt-3 rounded text-xs text-ide-muted hover:text-ide-accent" onClick={() => setSources(activity.sources)}>{t('recap.viewSources', { count: activity.sources.length })}</button>
            </div>}
          </article>)}
        </div> : !settings?.enabled || !hasContent || !day ? <Landing enabled={settings?.enabled} hasClosedPeriod={batches.length > 0} loading={(!settings || !day) && !loadError}
          error={loadError} running={running} onConfigure={configure} onGenerate={() => generate()} onRetry={refresh} />
          : <div className="recap-timeline">{batches.filter((batch) => batch.activities.length || batch.summary || batch.error || batch.summary_error || (batch.record_count ?? batch.records?.length) > 0).map((batch) => <Period key={batch.start_ms} batch={batch} time={time} onTask={enterTask} onDetails={() => setDiagnostics(true)} />)}</div>}
      </div>
    </div>}
    {editing && <CorrectionDialog key={`${editing.kind}:${editing.id || editing.task_id}`} date={date} editing={editing} threads={day?.threads || []} onClose={() => setEditing(null)} onSaved={(correction) => correctionSaved(correction, editing.epoch)} />}
    {sources && <SourcesDialog sources={sources} action={action} time={time} onClose={() => setSources(null)} />}
    {diagnostics && <DiagnosticsDialog date={date} onClose={() => setDiagnostics(false)} />}
    <ConfirmDialog isOpen={confirmDelete} title={t('recap.deleteTitle')} message={t('recap.deleteMessage', { date })}
      confirmLabel={t('recap.delete')} cancelLabel={t('common.cancel')} confirmVariant="danger"
      loading={operation === 'delete'} loadingLabel={t('recap.deleting')}
      onConfirm={remove} onCancel={() => setConfirmDelete(false)} />
  </div>;
}
