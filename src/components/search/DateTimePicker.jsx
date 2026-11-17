import React, { useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import { CalendarDays, ChevronLeft, ChevronRight } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Button } from '../ui/Button';
import { CalendarMonthSelect } from './CalendarMonthSelect';
import { TimeWheel } from './TimeWheel';
import './date-time-picker.css';

const pad = (value) => String(value).padStart(2, '0');
const dayKey = (date) => [String(date.getFullYear()).padStart(4, '0'), pad(date.getMonth() + 1), pad(date.getDate())].join('-');
const localValue = (date) => dayKey(date) + 'T' + pad(date.getHours()) + ':' + pad(date.getMinutes());
const parseValue = (value) => {
  if (!value) return null;
  const date = new Date(value);
  return Number.isFinite(date.getTime()) ? date : null;
};
const dayOf = (date) => {
  const result = new Date(date);
  result.setHours(0, 0, 0, 0);
  return result;
};
const monthOf = (date) => {
  const result = dayOf(date);
  result.setDate(1);
  return result;
};
const moveMonth = (date, amount) => {
  const result = monthOf(date);
  result.setMonth(result.getMonth() + amount);
  const last = new Date(result);
  last.setMonth(last.getMonth() + 1);
  last.setDate(0);
  result.setDate(Math.min(date.getDate(), last.getDate()));
  return result;
};
const POPOVER_GAP = 6;
const VIEWPORT_PADDING = 8;

/** A local-time calendar whose draft is applied only by its own confirmation. */
export function DateTimePicker({ label, value, onChange, min, max, invalidMessage, anchorRef }) {
  const { t, i18n } = useTranslation();
  const id = useId();
  const container = useRef(null);
  const trigger = useRef(null);
  const popover = useRef(null);
  const focusPending = useRef(false);
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState(null);
  const [selected, setSelected] = useState(() => dayOf(new Date()));
  const [viewMonth, setViewMonth] = useState(() => monthOf(new Date()));
  const [focusDay, setFocusDay] = useState(() => dayOf(new Date()));
  const [hour, setHour] = useState('0');
  const [minute, setMinute] = useState('0');
  const lower = parseValue(min);
  const upper = parseValue(max);
  const currentValue = parseValue(value);
  const locale = i18n?.resolvedLanguage || i18n?.language;
  const dayFormatter = new Intl.DateTimeFormat(locale, { dateStyle: 'full' });
  const monthFormatter = new Intl.DateTimeFormat(locale, { month: 'long' });
  const valueFormatter = new Intl.DateTimeFormat(locale, {
    year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hourCycle: 'h23',
  });

  const close = () => {
    setOpen(false);
    trigger.current?.focus({ preventScroll: true });
  };
  const canSelect = (date) => date.getFullYear() >= 1 && date.getFullYear() <= 9999
    && (!lower || date >= dayOf(lower)) && (!upper || date <= dayOf(upper));
  const canViewMonth = (date) => date.getFullYear() >= 1 && date.getFullYear() <= 9999
    && (!lower || monthOf(date) >= monthOf(lower)) && (!upper || monthOf(date) <= monthOf(upper));
  const showMonth = (date) => {
    const next = monthOf(date);
    if (lower && next < monthOf(lower)) next.setTime(monthOf(lower).getTime());
    if (upper && next > monthOf(upper)) next.setTime(monthOf(upper).getTime());
    setViewMonth(next);
    setFocusDay(lower && next < dayOf(lower) ? dayOf(lower) : next);
  };
  const openCalendar = () => {
    const initial = currentValue || new Date();
    const date = new Date(Math.max(lower?.getTime() ?? -Infinity, Math.min(upper?.getTime() ?? Infinity, initial.getTime())));
    setSelected(dayOf(date));
    setFocusDay(dayOf(date));
    setViewMonth(monthOf(date));
    setHour(pad(date.getHours()));
    setMinute(pad(date.getMinutes()));
    setPosition(null);
    focusPending.current = true;
    setOpen(true);
  };

  useEffect(() => {
    if (!open) return undefined;
    const outside = (event) => {
      if (!container.current?.contains(event.target)) setOpen(false);
    };
    document.addEventListener('mousedown', outside);
    return () => document.removeEventListener('mousedown', outside);
  }, [open]);

  useLayoutEffect(() => {
    if (!open) return undefined;
    const updatePosition = () => {
      if (!trigger.current || !popover.current) return;
      const input = trigger.current.getBoundingClientRect();
      const anchor = (anchorRef?.current || trigger.current).getBoundingClientRect();
      const { width, height } = popover.current.getBoundingClientRect();
      const right = window.innerWidth - VIEWPORT_PADDING;
      const bottom = window.innerHeight - VIEWPORT_PADDING;
      let left = input.right - width;
      let top = input.bottom + POPOVER_GAP;

      // Keep the calendar beside its range panel so both date fields stay visible.
      if (anchorRef?.current && anchor.right + POPOVER_GAP + width <= right) {
        left = anchor.right + POPOVER_GAP;
        top = anchor.top;
      } else if (anchorRef?.current && anchor.left - POPOVER_GAP - width >= VIEWPORT_PADDING) {
        left = anchor.left - POPOVER_GAP - width;
        top = anchor.top;
      } else if (top + height > bottom) {
        top = input.top - POPOVER_GAP - height;
      }

      left = Math.max(VIEWPORT_PADDING, Math.min(left, right - width));
      top = Math.max(VIEWPORT_PADDING, Math.min(top, bottom - height));
      setPosition((previous) => previous?.left === left && previous?.top === top ? previous : { left, top });
    };
    updatePosition();
    const observer = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(updatePosition);
    observer?.observe(popover.current);
    observer?.observe(anchorRef?.current || trigger.current);
    window.addEventListener('resize', updatePosition);
    document.addEventListener('scroll', updatePosition, true);
    return () => {
      observer?.disconnect();
      window.removeEventListener('resize', updatePosition);
      document.removeEventListener('scroll', updatePosition, true);
    };
  }, [open, anchorRef]);

  useLayoutEffect(() => {
    if (open && position && focusPending.current) {
      container.current?.querySelector('[data-date="' + dayKey(focusDay) + '"]')?.focus({ preventScroll: true });
      focusPending.current = false;
    }
  }, [open, focusDay, viewMonth, position]);

  const draft = new Date(selected);
  const numericTime = /^\d{1,2}$/.test(hour) && Number(hour) <= 23 && /^\d{1,2}$/.test(minute) && Number(minute) <= 59;
  draft.setHours(Number(hour), Number(minute), 0, 0);
  const validTime = numericTime && draft.getHours() === Number(hour) && draft.getMinutes() === Number(minute)
    && dayKey(draft) === dayKey(selected);
  const error = !validTime ? t('dateTimePicker.invalidTime')
    : (lower && draft < lower) || (upper && draft > upper)
      ? invalidMessage || t('dateTimePicker.outOfRange') : null;
  const applySelection = () => {
    if (error) return;
    onChange(localValue(draft));
    close();
  };
  const navigateMonth = (amount) => {
    const next = moveMonth(focusDay, amount);
    if (!canViewMonth(next)) return;
    showMonth(next);
  };
  const calendarKeyDown = (event, date) => {
    const next = new Date(date);
    const weekDay = (next.getDay() + 6) % 7;
    const days = { ArrowLeft: -1, ArrowRight: 1, ArrowUp: -7, ArrowDown: 7, Home: -weekDay, End: 6 - weekDay };
    if (event.key in days) next.setDate(next.getDate() + days[event.key]);
    else if (event.key === 'PageUp' || event.key === 'PageDown') {
      next.setTime(moveMonth(date, (event.key === 'PageUp' ? -1 : 1) * (event.shiftKey ? 12 : 1)).getTime());
    } else return;
    event.preventDefault();
    event.stopPropagation();
    if (!canSelect(next)) return;
    focusPending.current = true;
    setFocusDay(next);
    setViewMonth(monthOf(next));
  };

  const first = monthOf(viewMonth);
  first.setDate(1 - (first.getDay() + 6) % 7);
  const days = Array.from({ length: 42 }, (_, index) => {
    const date = new Date(first);
    date.setDate(first.getDate() + index);
    return date;
  });
  const today = dayKey(new Date());
  const weekdayFormatter = new Intl.DateTimeFormat(locale, { weekday: 'short' });
  const monday = new Date(2024, 0, 1);
  const weekdays = Array.from({ length: 7 }, (_, index) => {
    const date = new Date(monday);
    date.setDate(1 + index);
    return weekdayFormatter.format(date);
  });
  const monthOptions = Array.from({ length: 12 }, (_, month) => {
    const date = monthOf(viewMonth);
    date.setMonth(month);
    return { value: month, label: monthFormatter.format(date), disabled: !canViewMonth(date) };
  });

  return <div className="relative min-w-0 flex-1" ref={container}
    onKeyDown={(event) => {
      if (open && event.key === 'Escape') {
        event.preventDefault();
        event.stopPropagation();
        close();
      }
    }}>
    <button type="button" ref={trigger} aria-label={label} aria-haspopup="dialog"
      aria-expanded={open} aria-controls={open ? id : undefined}
      onClick={() => open ? close() : openCalendar()}
      className="flex w-full items-center justify-between gap-2 rounded border border-ide-border bg-ide-bg px-2 py-1.5 text-xs text-ide-text hover:border-ide-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/60">
      <span className={currentValue ? 'truncate tabular-nums' : 'truncate text-ide-muted'}>
        {currentValue ? valueFormatter.format(currentValue) : t('dateTimePicker.select')}
      </span>
      <CalendarDays className="h-3.5 w-3.5 shrink-0 text-ide-muted" aria-hidden="true" />
    </button>
    {open && <div ref={popover} id={id} role="dialog" aria-label={label} aria-describedby={error ? id + '-error' : undefined}
      className="fixed z-40 max-h-[calc(100vh-1rem)] w-[424px] max-w-[calc(100vw-1rem)] overflow-y-auto rounded-lg border border-ide-border bg-ide-panel p-3 text-ide-text shadow-xl custom-scrollbar"
      style={position || { visibility: 'hidden' }}
      onBlur={(event) => {
        if (event.relatedTarget && !event.currentTarget.contains(event.relatedTarget)) setOpen(false);
      }}
      onKeyDown={(event) => {
        if (event.key === 'Enter' && event.target.tagName === 'INPUT') {
          event.preventDefault();
          focusPending.current = true;
          setFocusDay(new Date(focusDay));
        }
      }}>
      <div className="grid grid-cols-[minmax(0,1fr)_100px] gap-4">
        <div className="min-w-0">
          <div className="mb-3 flex items-center gap-1.5">
            <Button variant="ghost" size="xs" className="h-8 w-6 p-0"
              aria-label={t('dateTimePicker.previousMonth')} onClick={() => navigateMonth(-1)}
              disabled={!canViewMonth(moveMonth(viewMonth, -1))}
              icon={<ChevronLeft className="h-4 w-4" />} />
            <div className="flex min-w-0 flex-1 items-center gap-1.5">
              <input type="number" min={lower?.getFullYear() || 1} max={upper?.getFullYear() || 9999} aria-label={t('dateTimePicker.year')}
                value={viewMonth.getFullYear()} onChange={(event) => {
                  const year = Number(event.target.value);
                  if (!Number.isInteger(year) || year < (lower?.getFullYear() || 1) || year > (upper?.getFullYear() || 9999)) return;
                  const next = monthOf(viewMonth);
                  next.setFullYear(year);
                  showMonth(next);
                }}
                className="date-time-picker-year h-8 w-24 shrink-0 rounded-md border border-ide-border bg-ide-bg px-2 py-1.5 text-sm tabular-nums focus:border-ide-accent focus:outline-none" />
              <CalendarMonthSelect label={t('dateTimePicker.month')} value={viewMonth.getMonth()} options={monthOptions} boundaryRef={popover}
                onChange={(month) => {
                  const next = monthOf(viewMonth);
                  next.setMonth(month);
                  if (canViewMonth(next)) showMonth(next);
                }} />
            </div>
            <Button variant="ghost" size="xs" className="h-8 w-6 p-0"
              aria-label={t('dateTimePicker.nextMonth')} onClick={() => navigateMonth(1)}
              disabled={!canViewMonth(moveMonth(viewMonth, 1))}
              icon={<ChevronRight className="h-4 w-4" />} />
          </div>
          <div role="grid" aria-label={t('dateTimePicker.calendar')}>
            <div role="row" className="grid grid-cols-7">
              {weekdays.map((day, index) => <span key={index} role="columnheader"
                className="pb-1 text-center text-[10px] text-ide-muted">{day}</span>)}
            </div>
            {Array.from({ length: 6 }, (_, week) => <div role="row" key={week} className="grid grid-cols-7">
              {days.slice(week * 7, week * 7 + 7).map((date) => {
                const key = dayKey(date);
                const isSelected = key === dayKey(selected);
                return <button type="button" role="gridcell" key={key} data-date={key}
                  aria-label={dayFormatter.format(date)} aria-selected={isSelected}
                  disabled={!canSelect(date)} tabIndex={key === dayKey(focusDay) ? 0 : -1}
                  onKeyDown={(event) => calendarKeyDown(event, date)}
                  onClick={() => {
                    focusPending.current = true;
                    setSelected(date);
                    setFocusDay(date);
                    setViewMonth(monthOf(date));
                  }}
                  className={['my-0.5 flex h-8 items-center justify-center rounded text-xs tabular-nums transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ide-accent/60 disabled:opacity-30',
                    isSelected ? 'bg-ide-accent font-medium text-white' : 'hover:bg-ide-hover',
                    !isSelected && date.getMonth() !== viewMonth.getMonth() ? 'text-ide-muted' : '',
                    !isSelected && key === today ? 'ring-1 ring-inset ring-ide-accent/50' : ''].join(' ')}>
                  {date.getDate()}
                </button>;
              })}
            </div>)}
          </div>
        </div>
        <div className="grid min-w-0 grid-cols-2 gap-1 border-l border-ide-border pl-3">
          <TimeWheel label={t('dateTimePicker.hour')} value={Number(hour)} max={23} onChange={(value) => setHour(pad(value))} />
          <TimeWheel label={t('dateTimePicker.minute')} value={Number(minute)} max={59} onChange={(value) => setMinute(pad(value))} />
        </div>
      </div>
      {error && <p id={id + '-error'} role="alert" className="mt-2 text-[11px] text-ide-error">{error}</p>}
      <div className="mt-3 flex items-center gap-1">
        <Button size="xs" variant="ghost" onClick={() => { onChange(''); close(); }}>{t('dateTimePicker.clear')}</Button>
        <Button size="xs" variant="ghost" disabled={!canSelect(dayOf(new Date()))} onClick={() => {
          const now = new Date();
          setSelected(dayOf(now));
          setFocusDay(dayOf(now));
          setViewMonth(monthOf(now));
          setHour(pad(now.getHours()));
          setMinute(pad(now.getMinutes()));
        }}>{t('dateTimePicker.today')}</Button>
        <div className="flex-1" />
        <Button size="xs" onClick={close}>{t('common.cancel')}</Button>
        <Button size="xs" variant="primary" onClick={applySelection} disabled={Boolean(error)}>{t('dateTimePicker.confirm')}</Button>
      </div>
    </div>}
  </div>;
}
