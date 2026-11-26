import React from 'react';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { createInstance } from 'i18next';
import { beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import en from '../../i18n/locales/en.json';
import zhCN from '../../i18n/locales/zh-CN.json';
import { moveUsageDate } from '../../lib/usage_api';
import UsageStatisticsView from './UsageStatisticsView';

const i18n = createInstance();
vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: i18n.t, i18n }) }));
beforeAll(() => i18n.init({ lng: 'en', resources: { en: { translation: en }, 'zh-CN': { translation: zhCN } }, interpolation: { escapeValue: false } }));
beforeEach(() => i18n.changeLanguage('en'));
const date = '2026-11-24';
const start = Date.UTC(2026, 10, 24);
const app = { id: 'endfield.exe', process_name: 'endfield.exe', name: 'Endfield', category: '游戏', total_ms: 3600000, visits: 1, longest_ms: 3600000 };
const day = { date, generation: 7, calendar: { start_ms: start, end_ms: start + 86400000, time_zone: 'UTC' },
  total_ms: 3600000, observed_ms: 7200000, first_ms: start, last_ms: start + 3600000, switches: 0, longest_ms: 3600000,
  apps: [app], categories: [{ category: '游戏', total_ms: 3600000 }], week: [{ date, total_ms: 3600000, observed_ms: 7200000 }],
  hours: Array.from({ length: 24 }, (_, index) => ({ start_ms: start + index * 3600000, end_ms: start + (index + 1) * 3600000, total_ms: index === 0 ? 3600000 : 0, categories: index === 0 ? [{ category: '游戏', total_ms: 3600000 }] : [] })),
  intervals: [{ state: 'active', app_id: app.id, category: '游戏', start_ms: start, end_ms: start + 3600000 }, { state: 'unknown', start_ms: start + 3600000, end_ms: start + 7200000 }],
};

describe('usage statistics hourly calendars', () => {
  it('shows UTC and the actual closing time when a time-zone change ends a date early', () => {
    const partialDay = { ...day,
      calendar: { ...day.calendar, end_ms: start + 22 * 3600000, time_zone: null,
        rule: 'system_time_zone_changed', start_utc_offset_seconds: 0, end_utc_offset_seconds: 0 },
      hours: day.hours.slice(0, 22),
    };
    render(<UsageStatisticsView date={date} today={date} day={partialDay} loading={false}
      selectDate={vi.fn()} selectApp={vi.fn()} clearApp={vi.fn()} refresh={vi.fn()} />);
    expect(screen.getByText(en.usage.timeZoneChanged)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /from 21:00 UTC to 22:00 UTC/ })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /24:00/ })).not.toBeInTheDocument();
  });
  it.each(['zh-CN', 'en'])('keeps numeric hour ticks and offset labels across DST changes in %s', async (locale) => {
    await i18n.changeLanguage(locale);
    const onViewSnapshots = vi.fn();
    const props = { today: date, loading: false, selectDate: vi.fn(), selectApp: vi.fn(), clearApp: vi.fn(), refresh: vi.fn(), onViewSnapshots };
    const transitions = [
      { date: '2026-03-08', start: '2026-03-08T05:00:00Z', end: '2026-03-09T04:00:00Z', offsets: [-18000, -14400], ticks: ['00', '07', '13', '19', '23'], before: 'GMT-5', after: 'GMT-4', nextHour: '03:00' },
      { date: '2026-11-01', start: '2026-11-01T04:00:00Z', end: '2026-11-02T05:00:00Z', offsets: [-14400, -18000], ticks: ['00', '05', '11', '17', '23'], before: 'GMT-4', after: 'GMT-5', nextHour: '01:00' },
    ];
    const { container, rerender } = render(<UsageStatisticsView {...props} date={date} day={day} />);
    for (const transition of transitions) {
      const startMs = Date.parse(transition.start); const endMs = Date.parse(transition.end);
      const hours = Array.from({ length: (endMs - startMs) / 3600000 }, (_, index) => ({ start_ms: startMs + index * 3600000, end_ms: startMs + (index + 1) * 3600000, total_ms: 0, categories: [] }));
      rerender(<UsageStatisticsView {...props} date={transition.date} day={{ ...day, date: transition.date, hours, first_ms: startMs, last_ms: startMs + 3600000, intervals: [],
        calendar: { start_ms: startMs, end_ms: endMs, time_zone: 'America/New_York', start_utc_offset_seconds: transition.offsets[0], end_utc_offset_seconds: transition.offsets[1] },
      }} />);
      const ticks = [...container.querySelectorAll('.usage-hour-tick')].map((tick) => tick.textContent).filter(Boolean);
      expect(ticks).toEqual(transition.ticks);
      const buttons = container.querySelectorAll('.usage-hour');
      expect(buttons[1]).toHaveAttribute('aria-label', expect.stringContaining(transition.before));
      expect(buttons[2]).toHaveAttribute('aria-label', expect.stringContaining(transition.after));
      expect(buttons[2]).toHaveAttribute('aria-label', expect.stringContaining(transition.nextHour));
      fireEvent.click(buttons[2]);
      expect(onViewSnapshots).toHaveBeenLastCalledWith(startMs + 2 * 3600000);
    }
  });
});

describe('usage statistics weekly totals', () => {
  it('keeps the selected week in weekday order and shows the date range when navigating to another week', () => {
    const today = '2026-11-26';
    const monday = '2026-11-23';
    const previousMonday = '2026-11-16';
    const previousSunday = '2026-11-22';
    const makeWeek = (first) => Array.from({ length: 7 }, (_, index) => {
      const date = moveUsageDate(first, index);
      return { date, total_ms: date <= today ? (index + 1) * 60000 : 0, observed_ms: date <= today ? 3600000 : 0 };
    });
    const selectDate = vi.fn(); const stepDate = vi.fn();
    const props = { today, loading: false, selectDate, stepDate, selectApp: vi.fn(), clearApp: vi.fn(), refresh: vi.fn() };
    const { container, rerender } = render(<UsageStatisticsView {...props} date={monday} day={{ ...day, date: monday, week: makeWeek(monday) }} />);
    const chart = within(container.querySelector('.usage-week'));
    const buttons = chart.getAllByRole('button');
    expect(chart.getByRole('heading', { name: 'Most recent week' })).toBeInTheDocument();
    expect(buttons.map((button) => button.textContent)).toEqual(['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun']);
    expect(buttons[0]).toHaveAttribute('aria-pressed', 'true');
    expect(buttons[3]).toHaveAccessibleName('4 min of use on Thursday, November 26, 2026');
    expect(buttons.slice(4).every((button) => button.disabled)).toBe(true);
    fireEvent.click(buttons[3]);
    expect(selectDate).toHaveBeenLastCalledWith(today);
    fireEvent.click(screen.getByRole('button', { name: 'Previous day' }));
    expect(stepDate).toHaveBeenLastCalledWith(-1);

    rerender(<UsageStatisticsView {...props} date={previousSunday} day={{ ...day, date: previousSunday, week: makeWeek(previousMonday) }} />);
    expect(chart.getByRole('heading', { name: /Nov 16\s*–\s*22/ })).toBeInTheDocument();
    expect(chart.getAllByRole('button')[6]).toHaveAttribute('aria-pressed', 'true');
    expect(chart.getAllByRole('button')[0]).toHaveAccessibleName('1 min of use on Monday, November 16, 2026');
  });
});

describe('usage statistics navigation', () => {
  it('delays the centered loading state, keeps the current layout and offers retry for the failed date', () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    try {
      const requestedDate = '2026-11-23';
      const refresh = vi.fn(); const stepDate = vi.fn();
      const props = { date, today: date, day, requestedDate, navigationDate: requestedDate, selectDate: vi.fn(), stepDate, refresh };
      const { container, rerender } = render(<UsageStatisticsView {...props} loading />);
      const plot = container.querySelector('.usage-hours');
      const panel = container.querySelector('.usage-panel');
      expect(screen.getByRole('button', { name: 'Tuesday, November 24, 2026' })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: 'Next day' })).toBeEnabled();
      act(() => vi.advanceTimersByTime(199));
      expect(screen.queryByRole('status')).not.toBeInTheDocument();
      expect(panel).toHaveAttribute('data-waiting', 'false');
      act(() => vi.advanceTimersByTime(1));
      expect(screen.getByRole('status')).toHaveTextContent('Loading usage statistics for Monday, November 23, 2026…');
      expect(panel).toHaveAttribute('data-waiting', 'true');
      expect(container.querySelector('.usage-hours')).toBe(plot);
      fireEvent.click(screen.getByRole('button', { name: 'Previous day' }));
      expect(stepDate).toHaveBeenCalledWith(-1);

      rerender(<UsageStatisticsView {...props} navigationDate={date} loading={false} error="USAGE_QUERY_FAILED" />);
      expect(screen.queryByRole('status')).not.toBeInTheDocument();
      expect(container.querySelector('.usage-hours')).toBe(plot);
      const alert = screen.getByRole('alert');
      expect(alert).toHaveTextContent('Monday, November 23, 2026');
      expect(alert).toHaveTextContent(en.usage.errors.USAGE_QUERY_FAILED);
      fireEvent.click(within(alert).getByRole('button', { name: en.common.retry }));
      expect(refresh).toHaveBeenCalledOnce();
    } finally {
      vi.useRealTimers();
    }
  });

  it('keeps initials behind transparent icons out of the tile and recovers after an icon source changes', () => {
    const icon = 'data:image/svg+xml,%3Csvg xmlns="http://www.w3.org/2000/svg" width="32" height="32"/%3E';
    const props = { date, today: date, loading: false, selectDate: vi.fn(), selectApp: vi.fn(), clearApp: vi.fn(), refresh: vi.fn() };
    const { container, rerender } = render(<UsageStatisticsView {...props} day={{ ...day, apps: [{ ...app, icon }] }} />);
    const tile = () => container.querySelector('.usage-app-tile');
    expect(tile()).toHaveTextContent('');
    expect(tile().querySelector('img')).toHaveAttribute('src', icon);

    fireEvent.error(tile().querySelector('img'));
    expect(tile().querySelector('img')).toBeNull();
    expect(tile()).toHaveTextContent('E');

    rerender(<UsageStatisticsView {...props} day={{ ...day, apps: [{ ...app, icon: 'updated-base64-icon' }] }} />);
    expect(tile()).toHaveTextContent('');
    expect(tile().querySelector('img')).toHaveAttribute('src', 'data:image/png;base64,updated-base64-icon');

    rerender(<UsageStatisticsView {...props} day={day} />);
    expect(tile().querySelector('img')).toBeNull();
    expect(tile()).toHaveTextContent('E');
  });

  it('opens the category control independently of app selection and saves a manual choice', async () => {
    const selectApp = vi.fn(); const saveCategory = vi.fn().mockResolvedValue(undefined);
    render(<UsageStatisticsView date={date} today={date} day={day} selectedApp={app}
      loading={false} selectDate={vi.fn()} selectApp={selectApp} clearApp={vi.fn()} refresh={vi.fn()} saveCategory={saveCategory} />);
    fireEvent.click(screen.getByRole('button', { name: 'Change category for Endfield' }));
    expect(selectApp).not.toHaveBeenCalled();
    expect(screen.getByRole('combobox')).toHaveValue('auto');
    fireEvent.change(screen.getByRole('combobox'), { target: { value: '游戏' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(saveCategory).toHaveBeenCalledWith(app.id, '游戏', 7);
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
  });

  it('restores automatic classification and keeps a failed change open for retry', async () => {
    const manual = { ...app, category_is_manual: true };
    const saveCategory = vi.fn().mockRejectedValueOnce(new Error('USAGE_SOURCE_CHANGED')).mockResolvedValue(undefined);
    render(<UsageStatisticsView date={date} today={date} day={{ ...day, apps: [manual] }} selectedApp={manual}
      loading={false} selectDate={vi.fn()} selectApp={vi.fn()} clearApp={vi.fn()} refresh={vi.fn()} saveCategory={saveCategory} />);
    fireEvent.click(screen.getByRole('button', { name: 'Change category for Endfield' }));
    expect(screen.getByRole('combobox')).toHaveValue('游戏');
    fireEvent.change(screen.getByRole('combobox'), { target: { value: 'auto' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(screen.getByText(en.usage.errors.USAGE_SOURCE_CHANGED)).toBeInTheDocument());
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(saveCategory).toHaveBeenLastCalledWith(app.id, null, 7);
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
  });

  it('preserves millisecond timestamps for nearby screenshots after removing the footer', () => {
    const onViewSnapshots = vi.fn(); const selectApp = vi.fn();
    render(<UsageStatisticsView date={date} today={date} day={day} selectedApp={app} details={{ titles: [{ label: 'High-load scene', total_ms: 3600000, first_ms: start + 1234 }] }}
      loading={false} selectDate={vi.fn()} selectApp={selectApp} clearApp={vi.fn()} refresh={vi.fn()} onViewSnapshots={onViewSnapshots} />);
    fireEvent.click(screen.getByRole('button', { name: /High-load scene/ }));
    expect(onViewSnapshots).toHaveBeenCalledWith(start + 1234);
    expect(screen.queryByRole('contentinfo')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Next day' })).toBeDisabled();
    expect(screen.getByText('Unrecorded')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Endfield Games' })).toHaveAttribute('aria-pressed', 'true');
  });
});
