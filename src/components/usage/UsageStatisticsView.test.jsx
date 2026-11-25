import React from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import UsageStatisticsView from './UsageStatisticsView';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key, i18n: { language: 'en' } }) }));
const date = '2026-11-24';
const start = Date.UTC(2026, 10, 24);
const app = { id: 'endfield.exe', process_name: 'endfield.exe', name: 'Endfield', category: '游戏', total_ms: 3600000, visits: 1, longest_ms: 3600000 };
const day = { date, calendar: { start_ms: start, end_ms: start + 86400000, time_zone: 'UTC' },
  total_ms: 3600000, observed_ms: 7200000, first_ms: start, last_ms: start + 3600000, switches: 0, longest_ms: 3600000,
  apps: [app], categories: [{ category: '游戏', total_ms: 3600000 }], week: [{ date, total_ms: 3600000, observed_ms: 7200000 }],
  hours: Array.from({ length: 24 }, (_, index) => ({ start_ms: start + index * 3600000, end_ms: start + (index + 1) * 3600000, total_ms: index === 0 ? 3600000 : 0, categories: index === 0 ? [{ category: '游戏', total_ms: 3600000 }] : [] })),
  intervals: [{ state: 'active', app_id: app.id, category: '游戏', start_ms: start, end_ms: start + 3600000 }, { state: 'unknown', start_ms: start + 3600000, end_ms: start + 7200000 }],
};

describe('usage statistics navigation', () => {
  it('preserves millisecond timestamps for nearby screenshots and the civil date for recaps', () => {
    const onViewSnapshots = vi.fn(); const onOpenRecap = vi.fn(); const selectApp = vi.fn();
    render(<UsageStatisticsView date={date} today={date} day={day} selectedApp={app} details={{ titles: [{ label: 'High-load scene', total_ms: 3600000, first_ms: start + 1234 }] }}
      loading={false} selectDate={vi.fn()} selectApp={selectApp} clearApp={vi.fn()} refresh={vi.fn()} onViewSnapshots={onViewSnapshots} onOpenRecap={onOpenRecap} />);
    fireEvent.click(screen.getByRole('button', { name: /High-load scene/ }));
    expect(onViewSnapshots).toHaveBeenCalledWith(start + 1234);
    fireEvent.click(screen.getByRole('button', { name: 'usage.openRecap' }));
    expect(onOpenRecap).toHaveBeenCalledWith({ date });
    expect(screen.getByRole('button', { name: 'usage.nextDay' })).toBeDisabled();
    expect(screen.getByText('usage.states.unknown')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Endfield usage.categories/ })).toHaveAttribute('aria-pressed', 'true');
  });
});
