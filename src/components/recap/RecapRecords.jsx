import React, { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { getRecapRecords, recapErrorKey } from '../../lib/recap_api';
import { Button } from '../ui/Button';
import { RecordList } from './RecapSources';

export default function RecapRecords({ date, batches, active, action, time, period, onPeriod, position }) {
  const { t } = useTranslation();
  const [state, setState] = useState({ items: [], total: 0, cursor: null, loaded: false, busy: false, error: '' });
  const request = useRef(() => {});
  const loadMore = useCallback(() => request.current(), []);
  const version = batches.map((b) => `${b.start_ms}:${b.updated_at_ms}:${b.record_count ?? b.records?.length}`).join('|');
  useEffect(() => {
    if (!active) return undefined;
    let live = true;
    let busy = false;
    let cursor = null;
    let done = false;
    let restarted = false;
    const load = async () => {
      if (!live || busy || done) return;
      busy = true;
      setState((old) => ({ ...old, busy: true, error: '' }));
      try {
        const page = await getRecapRecords(date, period === '' ? null : Number(period), cursor);
        if (!live) return;
        const first = cursor === null;
        cursor = page.next_cursor;
        done = !cursor;
        setState((old) => ({ items: first ? page.items : [...old.items, ...page.items], total: page.total, cursor, loaded: true, busy: false, error: '' }));
      } catch (error) {
        if (!live) return;
        if (String(error).includes('RECAP_SOURCE_CHANGED') && !restarted) {
          restarted = true; cursor = null;
          setState({ items: [], total: 0, cursor: null, loaded: false, busy: false, error: '' });
          busy = false; queueMicrotask(load); return;
        }
        const invalidated = String(error).includes('AUTH_REQUIRED') || String(error).includes('RECAP_SOURCE_CHANGED');
        if (invalidated) { cursor = null; done = false; }
        setState((old) => ({ ...old, ...(invalidated ? { items: [], total: 0, loaded: false } : {}), busy: false, error: String(error) }));
      } finally { busy = false; }
    };
    request.current = load;
    setState({ items: [], total: 0, cursor: null, loaded: false, busy: false, error: '' });
    load();
    return () => { live = false; request.current = () => {}; };
  }, [date, period, version, active]);
  return <section className="mx-auto flex h-full w-full max-w-[1120px] flex-col gap-4 px-6 py-5" aria-label={t('recap.records')}>
    <div className="flex shrink-0 flex-wrap items-center justify-between gap-3">
      <label className="flex items-center gap-3 text-xs text-ide-muted">{t('recap.period')}
        <select className="recap-focus rounded-lg border border-ide-border bg-ide-panel px-3 py-1.5 text-ide-text" value={period}
          onChange={(event) => { position.current = 0; onPeriod(event.target.value); }}>
          <option value="">{t('recap.allPeriods')}</option>
          {batches.map((batch) => <option key={batch.start_ms} value={String(batch.start_ms)}>{time(batch.start_ms)}–{time(batch.end_ms)}</option>)}
        </select>
      </label>
      <span role="status" className="text-xs tabular-nums text-ide-muted">{state.loaded ? t('recap.recordCount', { count: state.total }) : t('recap.loading')}</span>
    </div>
    {state.error && <div role="alert" className="flex items-center gap-3 text-xs text-ide-error">{t(recapErrorKey(state.error))}<Button onClick={loadMore}>{t('common.retry')}</Button></div>}
    {state.loaded && !state.total && <p className="py-8 text-sm text-ide-muted">{t('recap.noRecords')}</p>}
    {state.loaded && state.total > 0 && <RecordList key={`${date}:${period}`} items={state.items} total={state.total} action={action} time={time}
      label={t('recap.records')} offset={position.current} onOffset={(top) => { position.current = top; }} onNeedMore={state.error ? undefined : loadMore} />}
  </section>;
}
