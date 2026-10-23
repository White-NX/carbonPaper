import React, { useEffect, useId, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ArrowUpRight, ChevronDown } from 'lucide-react';
import { calendarTimeFormatter } from '../../lib/calendar_time';
import { recapErrorKey } from '../../lib/recap_api';
import useSnapshotRecap from '../../hooks/useSnapshotRecap';
import { Button } from '../ui/Button';

const taskKey = (item) => `${item.date}:${item.task_id}`;

function Activity({ item, screenshotId, expanded, onToggle, onOpenRecap, showHeading }) {
  const { t, i18n } = useTranslation();
  const id = useId();
  const details = useRef(null);
  const time = useMemo(() => calendarTimeFormatter(i18n.language, item.calendar), [i18n.language, item.calendar]);
  const range = `${time(item.start_ms)}${item.start_ms === item.end_ms ? '' : `–${time(item.end_ms)}`}`;
  useEffect(() => { if (details.current) details.current.inert = !expanded; }, [expanded]);

  return <div>
    <div className="mb-1 flex flex-wrap items-baseline justify-between gap-x-2 gap-y-1">
      {showHeading && <span className="text-xs font-medium text-ide-muted">{t('sidebar.recap.title')}</span>}
      <p className="ml-auto text-[11px] font-normal tabular-nums text-ide-muted" title={t('sidebar.recap.recordedTimeHint')}>{t('sidebar.recap.recordedTime', { time: range })}</p>
    </div>
    <Button variant="ghost" size="md" aria-expanded={expanded} aria-controls={id} onClick={onToggle}
      className="snapshot-recap-title -mx-1 w-[calc(100%+0.5rem)] items-start justify-between gap-2 rounded border-0 px-1 py-[3px] text-left text-[13px] font-normal leading-[1.65] whitespace-normal">
      <span className="min-w-0 break-words">{item.task_title}</span>
      <ChevronDown aria-hidden="true" className={`mt-1 h-3 w-3 shrink-0 text-ide-muted transition-transform duration-200 motion-reduce:transition-none ${expanded ? 'rotate-180' : ''}`} />
    </Button>
    <div ref={details} id={id} className="snapshot-recap-details" data-expanded={expanded} aria-hidden={!expanded}>
      <div className="min-h-0 overflow-hidden">
        <div className="snapshot-recap-details-body">
          <p className="whitespace-pre-wrap break-words text-[13px] font-normal leading-[1.75] text-ide-muted select-text">{item.text}</p>
          {onOpenRecap && <Button variant="ghost" size="xs" icon={ArrowUpRight} className="mt-2 -ml-2 font-normal"
            onClick={() => onOpenRecap({ date: item.date, task_id: item.task_id, activity_id: item.activity_id, screenshot_id: screenshotId })}>
            {t('sidebar.recap.openDetails')}
          </Button>}
        </div>
      </div>
    </div>
  </div>;
}

export default function SnapshotRecap({ screenshotId, active, isAuthenticated, onOpenRecap, onExpandedChange }) {
  const { t } = useTranslation();
  const { context, loading, error, refresh } = useSnapshotRecap(screenshotId, active, isAuthenticated);
  const [expandedTasks, setExpandedTasks] = useState(new Set());
  const items = context?.items || [];
  const expanded = items.some((item) => expandedTasks.has(taskKey(item)));

  useEffect(() => {
    if (!isAuthenticated) setExpandedTasks(new Set());
  }, [isAuthenticated]);
  useEffect(() => {
    if (!context) return;
    const keys = new Set(context.items.map(taskKey));
    setExpandedTasks((previous) => {
      const next = new Set([...previous].filter((key) => keys.has(key)));
      return next.size === previous.size ? previous : next;
    });
  }, [context]);
  useEffect(() => { onExpandedChange?.(expanded); }, [expanded, onExpandedChange]);
  useEffect(() => () => onExpandedChange?.(false), [onExpandedChange]);

  if (!active || !isAuthenticated || (!context && !loading && !error) || context?.status === 'unavailable') return null;
  const toggle = (item) => setExpandedTasks((previous) => {
    const next = new Set(previous);
    const key = taskKey(item);
    if (next.has(key)) next.delete(key); else next.add(key);
    return next;
  });

  //console.log('SnapshotRecap', { screenshotId, active, isAuthenticated, context, loading, error, items, expandedTasks });

  if (context?.status != 'ready' && !error && !loading) return;

  return <section aria-label={t('sidebar.recap.title')}>
    {!items.length && <div className="mb-1 text-xs font-medium text-ide-muted">{t('sidebar.recap.title')}</div>}
    <div className="space-y-3">
      {loading && <p role="status" className="text-xs text-ide-muted">{t('sidebar.recap.loading')}</p>}
      {error && <div role="alert" className="text-xs text-ide-error">
        <p>{t(recapErrorKey(error))}</p><Button variant="ghost" size="xs" className="mt-1 -ml-2" onClick={refresh}>{t('common.retry')}</Button>
      </div>}
      {context && items.map((item, index) => <Activity key={`${item.date}:${item.activity_id}`} item={item} screenshotId={screenshotId} showHeading={index === 0}
        expanded={expandedTasks.has(taskKey(item))} onToggle={() => toggle(item)} onOpenRecap={onOpenRecap} />)}
      {context && !items.length && <p className="text-xs text-ide-muted">{t(context.status === 'pending' ? 'sidebar.recap.pending' : 'sidebar.recap.unmatched')}</p>}
    </div>
  </section>;
}
