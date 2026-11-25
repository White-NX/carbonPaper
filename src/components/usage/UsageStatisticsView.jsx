import React, { useMemo, useState } from 'react';
import { ArrowLeft, BarChart3, CalendarDays, ChevronLeft, ChevronRight, Clock3, ExternalLink, RefreshCw } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { CATEGORY_COLORS, CATEGORY_LIST } from '../../lib/categories';
import { calendarTimeFormatter } from '../../lib/calendar_time';
import { moveUsageDate, usageDuration, usageErrorKey, usageHourlySelection } from '../../lib/usage_api';
import { PageHeader } from '../PageHeader';
import { Button } from '../ui/Button';
import { Banner } from '../ui/Banner';
import { DateTimePicker } from '../search/DateTimePicker';
import './usage-statistics.css';

const color = (category) => CATEGORY_COLORS[category] || '#84909f';
const categoryLabel = (category, t) => CATEGORY_LIST.includes(category) ? t(`usage.categories.${category}`) : category;
const dateValue = (date) => new Date(`${date}T12:00:00`);

function Total({ ms }) {
  const { t } = useTranslation();
  const minutes = Math.floor(ms / 60000);
  const hours = Math.floor(minutes / 60);
  if (ms > 0 && minutes === 0) return <div className="usage-total usage-total-small">{t('usage.lessThanMinute')}</div>;
  return <div className="usage-total">{hours > 0 && <><span>{hours}</span><small>{t('usage.hourUnit')}</small></>}<span>{minutes % 60}</span><small>{t('usage.minuteUnit')}</small></div>;
}

function Week({ days, date, today, selectDate }) {
  const { t, i18n } = useTranslation();
  const max = Math.max(1, ...days.map((day) => day.total_ms));
  const recorded = days.filter((day) => day.observed_ms > 0);
  const average = recorded.reduce((sum, day) => sum + day.total_ms, 0) / (recorded.length || 1);
  const weekday = new Intl.DateTimeFormat(i18n.language, { weekday: 'short' });
  return <div className="usage-week">
    <div className="usage-label"><h2>{t('usage.sevenDays')}</h2><span title={t('usage.averageHint')}>{recorded.length ? t('usage.dailyAverage', { duration: usageDuration(average, t) }) : t('usage.noAverage')}</span></div>
    <div className="usage-week-bars">
      {days.map((day) => <button key={day.date} type="button" className={`usage-week-day ${day.observed_ms ? '' : 'unrecorded'}`} disabled={day.date > today} aria-pressed={day.date === date}
        aria-label={`${day.date} · ${day.observed_ms ? usageDuration(day.total_ms, t) : t('usage.states.unknown')}`} title={`${day.date} · ${day.observed_ms ? usageDuration(day.total_ms, t) : t('usage.states.unknown')}`} onClick={() => selectDate(day.date)}>
        <span className="usage-week-column"><i style={{ height: `${day.total_ms / max * 100}%` }} /></span>
        <span className="usage-week-tick">{weekday.format(dateValue(day.date))}</span>
      </button>)}
    </div>
  </div>;
}

function Hours({ day, selectedApp, time, onViewSnapshots }) {
  const { t } = useTranslation();
  const selected = useMemo(() => usageHourlySelection(day, selectedApp?.id), [day, selectedApp?.id]);
  return <figure className="usage-hours-figure">
    <div className="usage-label"><figcaption>{t('usage.hourlyMinutes')}</figcaption><span>{selectedApp ? selectedApp.name : t('usage.allApps')}</span></div>
    <div className="usage-hours-axis"><span>60</span><span>30</span><span>0</span></div>
    <div className="usage-hours" style={{ '--usage-hours': day.hours.length }}>
      {day.hours.map((hour, index) => {
        const total = hour.total_ms;
        const end = hour.end_ms === day.calendar.end_ms ? t('usage.endOfDay') : time(hour.end_ms);
        const label = `${time(hour.start_ms)}–${end} · ${usageDuration(total, t)}${selectedApp ? ` · ${selectedApp.name} ${usageDuration(selected[index], t)}` : ''}`;
        const segments = selectedApp ? [{ category: null, total_ms: Math.max(0, total - selected[index]) }, { category: selectedApp.category, total_ms: selected[index] }] : hour.categories;
        return <button key={hour.start_ms} type="button" className="usage-hour" aria-label={label} title={label} onClick={() => onViewSnapshots?.(hour.start_ms)}>
          <span className="usage-hour-stack">{segments.filter((segment) => segment.total_ms > 0).map((segment, part) => <i key={`${segment.category}:${part}`}
            style={{ height: `${segment.total_ms / 3600000 * 100}%`, background: segment.category ? color(segment.category) : 'var(--ide-active)' }} />)}</span>
          <span className="usage-hour-tick">{index % 6 === 0 || index === day.hours.length - 1 ? time(hour.start_ms).slice(0, 2) : ''}</span>
        </button>;
      })}
    </div>
  </figure>;
}

function Ribbon({ day, selectedApp, selectApp, time }) {
  const { t } = useTranslation();
  const apps = new Map(day.apps.map((app) => [app.id, app]));
  const duration = day.calendar.end_ms - day.calendar.start_ms;
  const activeStyle = selectedApp ? { background: color(selectedApp.category) } : { background: 'linear-gradient(90deg,#3b82f6,#14b8a6,#f59e0b)' };
  return <div className="usage-ribbon-block">
    <div className="usage-ribbon" aria-label={t('usage.activityTimeline')}>
      {day.intervals.map((interval) => {
        const app = apps.get(interval.app_id);
        const label = `${app?.name || t(`usage.states.${interval.state}`)} · ${time(interval.start_ms)}–${interval.end_ms === day.calendar.end_ms ? t('usage.endOfDay') : time(interval.end_ms)} · ${usageDuration(interval.end_ms - interval.start_ms, t)}`;
        const style = { left: `${(interval.start_ms - day.calendar.start_ms) / duration * 100}%`, width: `${(interval.end_ms - interval.start_ms) / duration * 100}%`,
          ...(interval.state === 'active' ? { background: color(interval.category || app?.category), opacity: !selectedApp || selectedApp.id === interval.app_id ? 1 : 0.2 } : {}) };
        return interval.state === 'active' && app ? <button key={`${interval.start_ms}:${interval.end_ms}`} type="button" className="usage-segment active" style={style}
          aria-label={label} title={label} onClick={() => selectApp(app.id)} /> : <span key={`${interval.start_ms}:${interval.end_ms}`} className={`usage-segment ${interval.state}`} style={style} title={label} aria-label={label} role="img" />;
      })}
    </div>
    <div className="usage-legend">
      <span><i style={activeStyle} />{selectedApp ? selectedApp.name : t('usage.states.active')}</span>
      {selectedApp && <span><i className="other" />{t('usage.otherApps')}</span>}
      <span><i className="idle" />{t('usage.states.idle')}</span><span><i className="paused" />{t('usage.states.paused')}</span><span><i className="unknown" />{t('usage.states.unknown')}</span>
    </div>
  </div>;
}

function Apps({ apps, selectedApp, selectApp }) {
  const { t } = useTranslation();
  const max = Math.max(1, ...apps.map((app) => app.total_ms));
  return <section>
    <div className="usage-label"><h2>{t('usage.apps')}</h2><span>{t('usage.selectApp')}</span></div>
    {apps.length ? <div className="usage-app-list">{apps.map((app) => <button key={app.id} type="button" className="usage-app" aria-pressed={selectedApp?.id === app.id}
      style={{ '--usage-color': color(app.category) }} onClick={() => selectApp(app.id)}>
      <span className="usage-app-tile" aria-hidden="true">{app.name.slice(0, 1).toUpperCase()}{app.icon && <img src={app.icon.startsWith('data:') ? app.icon : `data:image/png;base64,${app.icon}`} alt="" onError={(event) => { event.currentTarget.hidden = true; }} />}</span>
      <span className="usage-app-name"><strong>{app.name}</strong><span>{categoryLabel(app.category, t)}</span></span>
      <span className="usage-app-duration">{usageDuration(app.total_ms, t)}</span>
      <span className="usage-track"><i style={{ width: `${app.total_ms / max * 100}%` }} /></span>
    </button>)}</div> : <p className="usage-empty">{t('usage.noApps')}</p>}
  </section>;
}

function Categories({ categories, total }) {
  const { t } = useTranslation();
  return <section className="usage-categories">
    <div className="usage-label"><h2>{t('usage.categoryTitle')}</h2></div>
    <div className="usage-category-strip" aria-hidden="true">{categories.map((item) => <i key={item.category} style={{ width: `${item.total_ms / Math.max(1, total) * 100}%`, background: color(item.category) }} />)}</div>
    {categories.map((item) => <div key={item.category} className="usage-category-row">
      <span><i style={{ background: color(item.category) }} />{categoryLabel(item.category, t)}</span>
      <span>{usageDuration(item.total_ms, t)}</span><span>{Math.round(item.total_ms / Math.max(1, total) * 100)}%</span>
    </div>)}
    {!categories.length && <p className="usage-empty">{t('usage.noCategories')}</p>}
  </section>;
}

function Details({ app, details, loading, error, onViewSnapshots, clearApp }) {
  const { t } = useTranslation();
  const [tab, setTab] = useState('websites');
  const [limit, setLimit] = useState(8);
  if (!app) return <section className="usage-detail-empty"><Clock3 className="h-5 w-5" aria-hidden="true" /><h2>{t('usage.singleApp')}</h2><p>{t('usage.detailsHint')}</p></section>;
  const websites = details?.websites || [];
  const websiteTab = websites.length > 0 && tab === 'websites';
  const list = websiteTab ? websites : details?.titles || [];
  return <section className="usage-details">
    <div className="usage-label"><h2>{app.name}</h2><Button variant="ghost" size="xs" onClick={clearApp} aria-label={t('usage.clearSelection')}>{t('usage.clearSelection')}</Button></div>
    {websites.length > 0 && <div className="usage-detail-tabs">
      <button type="button" aria-pressed={websiteTab} onClick={() => { setTab('websites'); setLimit(8); }}>{t('usage.websites')}</button>
      <button type="button" aria-pressed={!websiteTab} onClick={() => { setTab('titles'); setLimit(8); }}>{t('usage.windowTitles')}</button>
    </div>}
    {error ? <Banner tone="error">{t(usageErrorKey(error))}</Banner> : loading && !details ? <p role="status" className="usage-empty">{t('usage.loadingDetails')}</p> : <>
      {!websites.length && <h3 className="usage-detail-subtitle">{t('usage.windowTitles')}</h3>}
      {list.length ? <div className="usage-detail-list">{list.slice(0, limit).map((item) => <button key={item.label} type="button" className="usage-detail-row" onClick={() => onViewSnapshots?.(item.first_ms)}
        title={`${item.label} · ${t('usage.viewNearbySnapshots')}`}>
        <span className="usage-detail-name">{item.label}</span><span className="usage-detail-duration">{usageDuration(item.total_ms, t)}</span><ExternalLink className="h-3 w-3" aria-hidden="true" />
        <span className="usage-track"><i style={{ background: color(app.category), width: `${item.total_ms / Math.max(1, app.total_ms) * 100}%` }} /></span>
      </button>)}</div> : <p className="usage-empty">{t('usage.noDetails')}</p>}
      {list.length > limit && <Button variant="ghost" size="xs" onClick={() => setLimit((value) => value + 16)}>{t('usage.moreDetails', { count: list.length - limit })}</Button>}
      {app.category === '网页浏览' && !websites.length && <p className="usage-detail-note">{t('usage.extensionHint')}</p>}
    </>}
  </section>;
}

export default function UsageStatisticsView({ date, today, day, loading, error, refresh, selectDate, selectedApp, selectApp, clearApp, details, detailsError, detailsLoading, onViewSnapshots, onOpenRecap }) {
  const { t, i18n } = useTranslation();
  const time = useMemo(() => {
    const calendar = day?.calendar;
    if (calendar?.time_zone && calendar.start_utc_offset_seconds !== calendar.end_utc_offset_seconds) {
      const formatter = new Intl.DateTimeFormat(i18n.language, { hour: '2-digit', minute: '2-digit', hourCycle: 'h23', timeZone: calendar.time_zone, timeZoneName: 'shortOffset' });
      return (ms) => Number.isFinite(ms) ? formatter.format(new Date(ms)) : '';
    }
    return calendarTimeFormatter(i18n.language, calendar);
  }, [i18n.language, day?.calendar]);
  const fullDate = new Intl.DateTimeFormat(i18n.language, { year: 'numeric', month: 'long', day: 'numeric', weekday: 'short' }).format(dateValue(date));
  const scope = selectedApp ? t('usage.appDuration', { app: selectedApp.name }) : t(date === today ? 'usage.todayDuration' : 'usage.dayDuration');
  return <div className="usage-surface flex min-h-0 flex-1 flex-col overflow-hidden bg-ide-bg">
    <PageHeader as="header" className="usage-header">
      <div className="usage-header-layout grid min-w-0 flex-1 items-start gap-x-5 gap-y-2">
        <div className="flex h-10 min-w-0 items-center">
          <h1 className="flex min-w-0 items-center gap-2 text-sm font-semibold text-ide-text"><BarChart3 className="h-4 w-4 shrink-0 text-ide-accent" aria-hidden="true" /><span className="truncate">{t('usage.title')}</span></h1>
        </div>
        <div className="usage-header-actions flex min-h-10 items-center justify-end gap-1.5">
          {date !== today && <Button variant="ghost" onClick={() => selectDate(today)}>{t('usage.today')}</Button>}
          <Button variant="ghost" icon={RefreshCw} loading={loading} onClick={refresh} aria-label={t('usage.refresh')} title={t('usage.refresh')} />
        </div>
        <div role="group" aria-label={fullDate} className="usage-date-nav flex h-10 shrink-0 items-center gap-1.5">
          <Button variant="ghost" icon={ChevronLeft} onClick={() => selectDate(moveUsageDate(date, -1))} aria-label={t('usage.previousDay')} />
          <DateTimePicker label={fullDate} value={date} onChange={selectDate} max={today} dateOnly applyOnSelect clearable={false} className="w-36 flex-none" />
          <Button variant="ghost" icon={ChevronRight} onClick={() => selectDate(moveUsageDate(date, 1))} disabled={date >= today} aria-label={t('usage.nextDay')} />
        </div>
      </div>
    </PageHeader>
    <div className="usage-scroll custom-scrollbar">
      <div className="usage-body">
        {error && <Banner tone="error" action={<Button size="xs" onClick={refresh}>{t('common.retry')}</Button>}>{t(usageErrorKey(error))}</Banner>}
        {!day ? !error && <div className="usage-loading" role="status"><BarChart3 className="h-7 w-7" aria-hidden="true" /><p>{t('usage.loading')}</p></div> : <>
          <div className="usage-summary">
            <section className="usage-summary-main">
              <div className="usage-label"><h2>{scope}</h2>{selectedApp && <Button size="xs" variant="ghost" icon={ArrowLeft} onClick={clearApp}>{t('usage.allApps')}</Button>}</div>
              <Total ms={selectedApp?.total_ms ?? day.total_ms} />
              <div className="usage-facts">
                {selectedApp ? <><span>{t('usage.dayShare')} <b>{Math.round(selectedApp.total_ms / Math.max(1, day.total_ms) * 100)}%</b></span><span>{t('usage.visits')} <b>{selectedApp.visits}</b></span><span>{t('usage.longestVisit')} <b>{usageDuration(selectedApp.longest_ms, t)}</b></span></>
                  : <><span>{t('usage.firstUse')} <b>{time(day.first_ms) || '—'}</b></span><span>{t('usage.lastUse')} <b>{time(day.last_ms) || '—'}</b></span><span>{t('usage.appSwitches')} <b>{day.switches}</b></span><span>{t('usage.longestVisit')} <b>{usageDuration(day.longest_ms, t)}</b></span></>}
              </div>
            </section>
            <Week days={day.week} date={date} today={today} selectDate={selectDate} />
          </div>
          <Hours day={day} selectedApp={selectedApp} time={time} onViewSnapshots={onViewSnapshots} />
          <Ribbon day={day} selectedApp={selectedApp} selectApp={selectApp} time={time} />
          {!day.observed_ms && <Banner>{t('usage.noRecords')}{day.recorded_since_ms && <span> {t('usage.recordedSince', { date: new Intl.DateTimeFormat(i18n.language, { dateStyle: 'medium', timeZone: day.calendar.time_zone || undefined }).format(new Date(day.recorded_since_ms)) })}</span>}</Banner>}
          <div className="usage-columns"><Apps apps={day.apps} selectedApp={selectedApp} selectApp={selectApp} /><aside className="usage-side">
            <Details key={`${date}:${selectedApp?.id || ''}`} app={selectedApp} details={details} loading={detailsLoading} error={detailsError} onViewSnapshots={onViewSnapshots} clearApp={clearApp} />
            <Categories categories={day.categories} total={day.total_ms} />
          </aside></div>
          <footer className="usage-footer"><p>{t('usage.measurementHint')}</p><Button size="xs" variant="ghost" icon={CalendarDays} onClick={() => onOpenRecap?.({ date })}>{t('usage.openRecap')}</Button></footer>
        </>}
      </div>
    </div>
  </div>;
}
