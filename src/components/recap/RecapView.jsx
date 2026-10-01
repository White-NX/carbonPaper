import React, { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { listen } from '@tauri-apps/api/event';
import { CalendarDays, ChevronLeft, ChevronRight, Loader2, Settings2, Undo2, X } from 'lucide-react';
import { getAiSettings, grantAiRemoteConsent } from '../../lib/ai_api';
import { cancelRecap, correctRecap, generateRecap, getRecapDay, getRecapSettings, listRecapDays, localDate, recapErrorKey, saveRecapSettings, sourceResult } from '../../lib/recap_api';
import RecapSettings from './RecapSettings';
import RecapProgress from './RecapProgress';
import useRecapProgress from '../../hooks/useRecapProgress';

const buttonClass = 'rounded-lg border border-ide-border px-3 py-2 text-sm hover:bg-ide-hover disabled:opacity-40';
const inputClass = 'w-full rounded-lg border border-ide-border bg-ide-bg px-3 py-2 text-sm';

function RecapError({ error }) {
  const { t } = useTranslation();
  return <>
    <p>{t(recapErrorKey(error), { defaultValue: t('recap.errors.unknown') })}</p>
    <details className="mt-2 text-xs text-ide-muted">
      <summary className="cursor-pointer">{t('recap.errorDetails')}</summary>
      <pre className="mt-2 whitespace-pre-wrap break-words font-mono">{String(error)}</pre>
      <p className="mt-2">{t('recap.errorLogHint')}</p>
    </details>
  </>;
}

function RecapPeriod({ batch, task, time, busy, onTask, onEdit, onOpen }) {
  const { t } = useTranslation();
  const activities = batch.activities.filter((a) => !task || a.task_id === task);
  const topics = (batch.summary?.topics || []).filter((topic) => !task || topic.task_id === task);
  const titles = [...new Map(activities.map((a) => [a.task_id, a.task_title])).entries()];
  return <article className="rounded-xl border border-ide-border bg-ide-panel p-4 sm:p-5">
    <h2 className="text-sm font-semibold tabular-nums">{time(batch.start_ms)}–{time(batch.end_ms)}</h2>
    {!task && batch.summary && <p className="mt-3 text-sm leading-7">{batch.summary.overview}</p>}
    {topics.length > 0 ? <ul className="mt-4 space-y-4">{topics.map((topic) => <li key={topic.task_id}>
      <button className="text-left text-sm font-medium text-ide-accent hover:underline" onClick={() => onTask(topic.task_id)}>{topic.title}</button>
      <p className="mt-1 text-sm leading-7">{topic.text}</p>
    </li>)}</ul> : <>
      <p className="mt-3 text-xs text-ide-muted">{t(batch.summary_error ? 'recap.summaryUnavailable' : batch.summary ? 'recap.taskInDetails' : 'recap.summaryPending')}</p>
      <div className="mt-3 flex flex-wrap gap-3">{titles.map(([id, title]) => <button key={id} className="text-sm text-ide-accent hover:underline" onClick={() => onTask(id)}>{title}</button>)}</div>
    </>}
    <details className="mt-4"><summary className="cursor-pointer text-xs text-ide-muted">{t('recap.details')}</summary>
      <div className="mt-3 space-y-4">{activities.map((activity) => <div key={activity.id} className="border-l-2 border-ide-border pl-3">
        <div className="flex flex-wrap items-start justify-between gap-2 text-xs text-ide-muted">
          <span>{time(activity.start_ms)}{activity.end_ms !== activity.start_ms && `–${time(activity.end_ms)}`}</span>
          <button onClick={() => onEdit(activity)} disabled={busy} className="text-ide-accent">{t('recap.correct')}</button>
        </div>
        <button className="mt-2 text-left text-sm font-medium hover:underline" onClick={() => onTask(activity.task_id)}>{activity.task_title}</button>
        <p className="mt-1 whitespace-pre-wrap text-sm leading-7">{activity.text}</p>
        <div className="mt-2 flex flex-wrap gap-2">{activity.sources.map((source) => <button key={source.id} className="max-w-64 truncate rounded border border-ide-border px-2 py-1 text-xs hover:bg-ide-hover" onClick={() => onOpen(source, true)} title={source.window_title}>{time(source.timestamp_ms)} · {source.process_name || t('recap.source')}</button>)}</div>
        {activity.sources.length > 0 && <button className="mt-2 text-xs text-ide-accent" onClick={() => onOpen(activity.sources[0])}>{t('recap.locate')}</button>}
      </div>)}</div>
    </details>
  </article>;
}

function CorrectionForm({ editing, threads, busy, onApply, onClose }) {
  const { t } = useTranslation();
  const [kind, setKind] = useState('rename');
  const [title, setTitle] = useState(editing.task_title);
  const [target, setTarget] = useState('');
  const submit = (event) => {
    event.preventDefault();
    if (kind === 'rename') onApply({ kind, task_id: editing.task_id, title });
    if (kind === 'merge') onApply({ kind, from: editing.task_id, into: target });
    if (kind === 'move') onApply({ kind, activity_id: editing.id, task_id: target || null, title });
  };
  return <form onSubmit={submit} className="space-y-3 rounded-lg border border-ide-border bg-ide-panel p-4" aria-label={t('recap.correct')}>
    <label className="block text-xs"><span>{t('recap.correction.action')}</span><select className={inputClass} value={kind} onChange={(e) => setKind(e.target.value)}>{['rename', 'merge', 'move'].map((k) => <option key={k} value={k}>{t(`recap.correction.${k}`)}</option>)}</select></label>
    {kind !== 'rename' && <label className="block text-xs"><span>{t('recap.correction.target')}</span><select className={inputClass} value={target} required={kind === 'merge'} onChange={(e) => setTarget(e.target.value)}><option value="">{t(kind === 'move' ? 'recap.correction.newTask' : 'recap.correction.choose')}</option>{threads.filter((thread) => thread.id !== editing.task_id).map((thread) => <option key={thread.id} value={thread.id}>{thread.title}</option>)}</select></label>}
    {(kind === 'rename' || (kind === 'move' && !target)) && <label className="block text-xs"><span>{t('recap.correction.title')}</span><input className={inputClass} required maxLength={120} value={title} onChange={(e) => setTitle(e.target.value)} /></label>}
    <div className="flex justify-end gap-2"><button type="button" onClick={onClose} className={buttonClass}>{t('recap.close')}</button><button disabled={busy} className={buttonClass}>{t('recap.save')}</button></div>
  </form>;
}

export default function RecapView({ active, isAuthenticated, onSelectScreenshot, onOpenSnapshotPreview }) {
  const { t, i18n } = useTranslation();
  const [date, setDate] = useState(localDate);
  const [day, setDay] = useState(null);
  const [settings, setSettings] = useState(null);
  const [ai, setAi] = useState(null);
  const [days, setDays] = useState([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [loadError, setLoadError] = useState('');
  const [showSettings, setShowSettings] = useState(false);
  const [task, setTask] = useState(null);
  const [editing, setEditing] = useState(null);
  const { progress, error: progressError } = useRecapProgress(date, active, isAuthenticated);
  const current = useRef({ date, active, isAuthenticated });
  current.current = { date, active, isAuthenticated };
  const time = (ms) => new Date(ms).toLocaleTimeString(i18n.language, { hour: '2-digit', minute: '2-digit' });
  const load = useCallback(async () => {
    const requestDate = current.current.date;
    if (!current.current.active || !current.current.isAuthenticated) return;
    try {
      const result = await getRecapDay(requestDate);
      if (current.current.date === requestDate && current.current.isAuthenticated) { setDay(result); setLoadError(''); }
    } catch (e) { if (current.current.date === requestDate) { setDay(null); setLoadError(String(e)); } }
  }, []);
  useEffect(() => {
    setDay(null); setTask(null); setEditing(null); setError(''); setLoadError('');
    if (!active || !isAuthenticated) { if (!isAuthenticated) { setSettings(null); setAi(null); } return undefined; }
    let live = true;
    Promise.all([getRecapSettings(), getAiSettings(), listRecapDays()]).then(([s, a, dates]) => { if (live) { setSettings(s); setAi(a); setDays(dates); } }).catch((e) => { if (live) setError(String(e)); });
    load();
    const timer = setInterval(load, 15000);
    window.addEventListener('focus', load);
    const subscription = listen('recap-changed', load);
    return () => { live = false; clearInterval(timer); window.removeEventListener('focus', load); subscription.then((unlisten) => unlisten()); };
  }, [active, isAuthenticated, date, load]);

  const run = async (work) => {
    setBusy(true); setError('');
    try { await work(); } catch (e) { setError(String(e)); } finally { setBusy(false); }
  };
  const generate = (force = false) => run(async () => {
    const requested = date;
    await generateRecap(requested, force);
    await load();
  });
  const apply = (correction) => run(async () => { await correctRecap(date, correction); setEditing(null); setTask(null); await load(); });
  const save = (draft) => run(async () => { setSettings(await saveRecapSettings(draft)); setShowSettings(false); await load(); });
  const shift = (delta) => { const d = new Date(`${date}T12:00:00`); d.setDate(d.getDate() + delta); setDate(localDate(d)); };
  const open = (source, floating = false) => {
    const result = sourceResult(source);
    if (floating && onOpenSnapshotPreview) onOpenSnapshotPreview(result, { sourceLabel: t('recap.title'), sourceType: 'recap' });
    else onSelectScreenshot?.(result);
  };
  const periods = (day?.batches || []).filter((b) => b.activities.some((a) => !task || a.task_id === task));
  const running = busy || day?.running || (progress && !progress.finished_at_ms);
  const issue = error || loadError || progressError || day?.error || '';
  const batchIssues = day?.batches.filter((batch) => batch.error || batch.summary_error || batch.status === 'failed') || [];
  const consentNeeded = issue.includes('AI_REMOTE_CONSENT_REQUIRED') || day?.batches.some((b) => b.error === 'AI_REMOTE_CONSENT_REQUIRED' || b.summary_error === 'AI_REMOTE_CONSENT_REQUIRED');

  if (!isAuthenticated) return <div className="m-auto p-8 text-sm text-ide-muted">{t('recap.locked')}</div>;
  return <div className="flex min-h-0 flex-1 flex-col text-ide-text">
    <header className="flex flex-wrap items-center justify-between gap-3 border-b border-ide-border px-5 py-4">
      <div className="flex items-center gap-2"><CalendarDays className="h-5 w-5 text-ide-accent" /><h1 className="text-base font-semibold">{t('recap.title')}</h1></div>
      <div className="flex flex-wrap items-center gap-2">
        <button className={buttonClass} aria-label={t('recap.previous')} onClick={() => shift(-1)}><ChevronLeft size={16} /></button>
        <input type="date" aria-label={t('recap.date')} max={localDate()} value={date} onChange={(e) => { if (e.target.value) setDate(e.target.value); }} className="rounded-lg border border-ide-border bg-ide-panel px-2 py-1.5 text-sm" list="recap-dates" />
        <datalist id="recap-dates">{days.map((d) => <option key={d} value={d} />)}</datalist>
        <button className={buttonClass} aria-label={t('recap.next')} disabled={date >= localDate()} onClick={() => shift(1)}><ChevronRight size={16} /></button>
        <button className={buttonClass} aria-label={t('recap.settings.title')} disabled={!settings} onClick={() => setShowSettings((v) => !v)}><Settings2 size={16} /></button>
      </div>
    </header>
    <div className="min-h-0 flex-1 overflow-y-auto p-5"><div className="mx-auto max-w-4xl space-y-5">
      {showSettings && settings && <RecapSettings settings={settings} providers={ai?.providers || []} busy={busy} onSave={save} onClose={() => setShowSettings(false)} usage={day?.usage} />}
      {(issue || consentNeeded) && <div role="alert" className="rounded-lg border border-ide-border p-3 text-sm"><RecapError error={issue || 'AI_REMOTE_CONSENT_REQUIRED'} />{consentNeeded && <div className="mt-3 space-y-2"><p>{t('recap.remoteConsent')}</p><button className={buttonClass} disabled={busy} onClick={() => run(async () => { setAi(await grantAiRemoteConsent()); await generateRecap(date); await load(); })}>{t('recap.allowRemote')}</button></div>}</div>}
      {!settings?.enabled ? <div className="rounded-xl border border-ide-border bg-ide-panel p-6"><h2 className="font-medium">{t('recap.welcome')}</h2><p className="mt-2 text-sm leading-relaxed text-ide-muted">{t('recap.intro')}</p><button className={`${buttonClass} mt-4`} disabled={!settings} onClick={() => setShowSettings(true)}>{t('recap.configure')}</button></div> : <>
        <div className="flex flex-wrap items-center justify-between gap-3 text-sm">
          <p className="text-ide-muted">{t(running ? 'recap.generating' : 'recap.schedule')}</p>
          <div className="flex flex-wrap gap-2">
            {day?.can_undo && <button className={buttonClass} disabled={busy} onClick={() => apply({ kind: 'undo' })}><Undo2 className="mr-1 inline h-4 w-4" />{t('recap.undo')}</button>}
            {running ? <button className={buttonClass} onClick={() => cancelRecap().catch((e) => setError(String(e)))}>{t('recap.stop')}</button> : <><button className={buttonClass} onClick={() => generate()}>{t('recap.generate')}</button>{periods.length > 0 && <button className={buttonClass} onClick={() => generate(true)}>{t('recap.regenerate')}</button>}</>}
          </div>
        </div>
        {task && <div className="flex items-center justify-between rounded-lg bg-ide-panel px-4 py-2 text-sm"><span>{day?.threads.find((thread) => thread.id === task)?.title}</span><button onClick={() => setTask(null)} aria-label={t('recap.allActivities')}><X size={16} /></button></div>}
        {editing && <CorrectionForm key={editing.id} editing={editing} threads={day?.threads || []} busy={busy} onApply={apply} onClose={() => setEditing(null)} />}
        {running && !progress && <div className="flex items-center gap-2 text-sm text-ide-muted"><Loader2 className="h-4 w-4 animate-spin" />{t('recap.generating')}</div>}
        <RecapProgress progress={progress} batches={day?.batches || []} />
        {batchIssues.map((batch) => <div key={batch.start_ms} role="alert" className="rounded-lg border border-ide-border bg-ide-panel p-4 text-sm">
          <p className="mb-2 font-medium">{time(batch.start_ms)}–{time(batch.end_ms)} · {t(batch.status === 'failed' ? 'recap.failed' : 'recap.partial')}</p>
          <RecapError error={batch.error || batch.summary_error || 'RECAP_INVALID_RESPONSE'} />
        </div>)}
        {!running && !issue && batchIssues.length === 0 && periods.length === 0 && <p className="py-8 text-center text-sm text-ide-muted">{t('recap.empty')}</p>}
        <div className="space-y-3">{periods.map((batch) => <RecapPeriod key={batch.start_ms} batch={batch} task={task} time={time} busy={busy} onTask={setTask} onEdit={setEditing} onOpen={open} />)}</div>
        {day?.batches.map((batch) => <div key={batch.start_ms}>
          {batch.status === 'partial' && !batch.error && <p className="text-xs text-ide-muted">{time(batch.start_ms)}–{time(batch.end_ms)} · {t('recap.partial')}</p>}
          {batch.records.length > 0 && <details className="mt-2 text-xs text-ide-muted"><summary className="cursor-pointer">{t('recap.allRecords', { start: time(batch.start_ms), end: time(batch.end_ms), count: batch.records.length })}</summary><div className="mt-2 grid max-h-64 gap-1 overflow-y-auto sm:grid-cols-2">{batch.records.map((source) => <button key={source.id} onClick={() => open(source, true)} className="truncate rounded px-2 py-1 text-left hover:bg-ide-hover">{time(source.timestamp_ms)} · {source.window_title || source.process_name || t('recap.source')}</button>)}</div></details>}
        </div>)}
      </>}
    </div></div>
  </div>;
}
