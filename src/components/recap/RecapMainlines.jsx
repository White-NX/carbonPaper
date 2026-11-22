import React, { useId, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ChevronDown, ChevronRight, ChevronUp, GitBranch, Images } from 'lucide-react';
import { Button } from '../ui/Button';
import { Banner } from '../ui/Banner';
import RecapWarnings from './RecapWarnings';
import RecapBudgetNotice from './RecapBudgetNotice';
import { recapBudgetIssue } from '../../lib/recap_api';

const VISIBLE_TASKS = 3;

function TaskBranch({ task, itemId, batches, time, onTask, preview = false }) {
  const periods = batches.filter(batch => batch.activities.some(activity => activity.task_id === task.id));
  const rowClassName = 'flex w-full min-w-0 items-center gap-3 px-2.5 py-2 text-left';
  const content = <>
    <span className="min-w-0 flex-1"><span className="block break-words text-sm font-medium">{task.title}</span>
      {periods.length > 0 && <span className="mt-1 block break-words text-xs leading-relaxed tabular-nums text-ide-muted">{periods.map(batch => `${time(batch.start_ms)}–${time(batch.end_ms)}`).join(' · ')}</span>}
    </span>
    <ChevronRight aria-hidden="true" className="h-4 w-4 shrink-0 text-ide-muted" />
  </>;
  return <li aria-hidden={preview || undefined} className={`recap-mainline-branch${preview ? ' recap-mainline-preview' : ''}`}>
    {preview ? <div className={rowClassName}>{content}</div> : <button type="button" data-recap-task={task.id} data-recap-entry={`mainline:${itemId}:${task.id}`}
      onClick={() => onTask(task.id, `mainline:${itemId}`)}
      className={`recap-mainline-action recap-focus ${rowClassName}`}>{content}</button>}
  </li>;
}

function Mainline({ item, threads, batches, time, onTask, onSources, returnEntry }) {
  const { t } = useTranslation();
  const id = useId();
  const tasks = item.task_ids.map(taskId => threads.find(thread => thread.id === taskId)).filter(Boolean);
  const [expanded, setExpanded] = useState(() => tasks.slice(VISIBLE_TASKS).some(task => returnEntry === `mainline:${item.id}:${task.id}`));
  const extra = tasks.length - VISIBLE_TASKS;
  return <article aria-labelledby={`${id}-title`} className="recap-mainline-grid min-w-0 border-b border-ide-border/60 py-5 first:pt-0 last:border-b-0 last:pb-0">
    <span aria-hidden="true" className="recap-mainline-marker" />
    <div className="min-w-0">
      <h3 id={`${id}-title`} className="w-full min-w-0 break-words px-2.5 py-2 text-[15px] font-semibold leading-relaxed">{item.title}</h3>
      <p className="mx-2.5 mt-2 whitespace-pre-wrap break-words text-sm leading-7 text-ide-muted">{item.text}</p>
      <RecapWarnings warnings={item.warnings} className="mx-2.5 mt-3" />
    </div>
    <div className="recap-mainline-branches min-w-0">
      <p className="mb-2 pl-8 text-xs text-ide-muted">{t('recap.mainlines.taskCount', { count: tasks.length })}</p>
      <ul id={`${id}-tasks`}>
        {(expanded ? tasks : tasks.slice(0, VISIBLE_TASKS)).map(task => <TaskBranch key={task.id} task={task} itemId={item.id} batches={batches} time={time} onTask={onTask} />)}
        {extra > 0 && !expanded && <TaskBranch task={tasks[VISIBLE_TASKS]} batches={batches} time={time} preview />}
      </ul>
      {extra > 0 && <div><Button variant="ghost" size="xs" icon={expanded ? ChevronUp : ChevronDown}
        className="recap-mainline-action ml-[22px] mt-1 whitespace-normal border-0 px-2.5 py-2 text-left"
        aria-expanded={expanded} aria-controls={`${id}-tasks`} onClick={() => setExpanded(value => !value)}>
        {t(expanded ? 'recap.mainlines.collapseTasks' : 'recap.mainlines.expandTasks', { count: extra })}
      </Button></div>}
      {item.sources?.length > 0 && <Button variant="ghost" size="xs" icon={Images}
        className="recap-mainline-action ml-[22px] mt-1 border-0 px-2.5 py-2 text-ide-accent hover:text-ide-accent"
        onClick={() => onSources(item.sources)}>{t('recap.mainlines.sources')}</Button>}
    </div>
  </article>;
}

export default function RecapMainlines({ mainlines, error, pending = false, threads, batches, time, onTask, onSources, onDetails, onAdjust, returnEntry }) {
  const { t } = useTranslation();
  const items = mainlines?.items || [];
  if (mainlines && !items.length && !error && !pending) return null;
  if (!items.length && !batches.some(batch => batch.activities.length)) return null;
  return <section aria-label={t('recap.mainlines.title')} className="recap-mainlines mb-9 min-w-0">
    <div className="mb-4 flex items-center justify-between gap-4">
      <h2 className="flex items-center gap-2 text-sm font-semibold"><GitBranch aria-hidden="true" className="h-4 w-4 text-ide-accent" />{t('recap.mainlines.title')}</h2>
      {items.length > 0 && <span className="shrink-0 text-xs text-ide-muted">{t('recap.mainlines.count', { count: items.length })}</span>}
    </div>
    {recapBudgetIssue(error) ? <RecapBudgetNotice error={error} target="recap.mainlines.title" onAdjust={onAdjust} className="mb-4" />
      : error && <Banner tone="warning" className="mb-4" action={<Button variant="ghost" size="xs" onClick={onDetails}>{t('recap.generationDetails')}</Button>}>{t(items.length ? 'recap.mainlines.updateFailed' : 'recap.mainlines.unavailable')}</Banner>}
    {pending && items.length > 0 && !error && <Banner tone="info" className="mb-4">{t('recap.mainlines.updatesPending')}</Banner>}
    {items.length ? <div>{items.map((item) => <Mainline key={item.id} item={item} threads={threads} batches={batches} time={time} onTask={onTask} onSources={onSources} returnEntry={returnEntry} />)}</div>
      : !error && <p className="text-xs leading-relaxed text-ide-muted">{t('recap.mainlines.pending')}</p>}
  </section>;
}
