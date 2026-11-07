import React, { useId } from 'react';
import { useTranslation } from 'react-i18next';
import { ChevronRight, GitBranch, Images } from 'lucide-react';
import { Button } from '../ui/Button';
import { Banner } from '../ui/Banner';

function Mainline({ item, threads, batches, time, onTask, onSources }) {
  const { t } = useTranslation();
  const id = useId();
  const tasks = item.task_ids.map(taskId => threads.find(thread => thread.id === taskId)).filter(Boolean);
  return <article aria-labelledby={`${id}-title`} className="recap-mainline-grid min-w-0 border-b border-ide-border/60 py-5 first:pt-0 last:border-b-0 last:pb-0">
    <span aria-hidden="true" className="recap-mainline-marker" />
    <div className="min-w-0">
      <h3 id={`${id}-title`} className="w-full min-w-0 break-words px-2.5 py-2 text-[15px] font-semibold leading-relaxed">{item.title}</h3>
      <p className="mx-2.5 mt-2 whitespace-pre-wrap break-words text-sm leading-7 text-ide-muted">{item.text}</p>
    </div>
    <div className="recap-mainline-branches min-w-0">
      <p className="mb-2 pl-8 text-xs text-ide-muted">{t('recap.mainlines.taskCount', { count: tasks.length })}</p>
      <ul>
        {tasks.map(task => {
          const periods = batches.filter(batch => batch.activities.some(activity => activity.task_id === task.id));
          return <li key={task.id} className="recap-mainline-branch">
            <button type="button" data-recap-task={task.id} data-recap-entry={`mainline:${item.id}:${task.id}`}
              onClick={() => onTask(task.id, `mainline:${item.id}`)}
              className="recap-mainline-action recap-focus flex w-full min-w-0 items-center gap-3 px-2.5 py-2 text-left">
              <span className="min-w-0 flex-1"><span className="block break-words text-sm font-medium">{task.title}</span>
                {periods.length > 0 && <span className="mt-1 block break-words text-xs leading-relaxed tabular-nums text-ide-muted">{periods.map(batch => `${time(batch.start_ms)}–${time(batch.end_ms)}`).join(' · ')}</span>}
              </span>
              <ChevronRight aria-hidden="true" className="h-4 w-4 shrink-0 text-ide-muted" />
            </button>
          </li>;
        })}
      </ul>
      {item.sources?.length > 0 && <Button variant="ghost" size="xs" icon={Images}
        className="recap-mainline-action ml-[22px] mt-1 border-0 px-2.5 py-2 text-ide-accent hover:text-ide-accent"
        onClick={() => onSources(item.sources)}>{t('recap.mainlines.sources')}</Button>}
    </div>
  </article>;
}

export default function RecapMainlines({ mainlines, error, threads, batches, time, onTask, onSources, onDetails }) {
  const { t } = useTranslation();
  const items = mainlines?.items || [];
  if (mainlines && !items.length && !error) return null;
  if (!items.length && !batches.some(batch => batch.activities.length)) return null;
  return <section aria-label={t('recap.mainlines.title')} className="recap-mainlines mb-9 min-w-0">
    <div className="mb-4 flex items-center justify-between gap-4">
      <h2 className="flex items-center gap-2 text-sm font-semibold"><GitBranch aria-hidden="true" className="h-4 w-4 text-ide-accent" />{t('recap.mainlines.title')}</h2>
      {items.length > 0 && <span className="shrink-0 text-xs text-ide-muted">{t('recap.mainlines.count', { count: items.length })}</span>}
    </div>
    {error && <Banner tone="warning" className="mb-4" action={<Button variant="ghost" size="xs" onClick={onDetails}>{t('recap.generationDetails')}</Button>}>{t('recap.mainlines.unavailable')}</Banner>}
    {items.length ? <div>{items.map((item) => <Mainline key={item.id} item={item} threads={threads} batches={batches} time={time} onTask={onTask} onSources={onSources} />)}</div>
      : !error && <p className="text-xs leading-relaxed text-ide-muted">{t('recap.mainlines.pending')}</p>}
  </section>;
}
