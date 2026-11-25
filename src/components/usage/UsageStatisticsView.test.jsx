import React from 'react';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import UsageStatisticsView from './UsageStatisticsView';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key, i18n: { language: 'en' } }) }));
const date = '2026-11-24';
const start = Date.UTC(2026, 10, 24);
const app = { id: 'endfield.exe', process_name: 'endfield.exe', name: 'Endfield', category: '游戏', total_ms: 3600000, visits: 1, longest_ms: 3600000 };
const day = { date, generation: 7, calendar: { start_ms: start, end_ms: start + 86400000, time_zone: 'UTC' },
  total_ms: 3600000, observed_ms: 7200000, first_ms: start, last_ms: start + 3600000, switches: 0, longest_ms: 3600000,
  apps: [app], categories: [{ category: '游戏', total_ms: 3600000 }], week: [{ date, total_ms: 3600000, observed_ms: 7200000 }],
  hours: Array.from({ length: 24 }, (_, index) => ({ start_ms: start + index * 3600000, end_ms: start + (index + 1) * 3600000, total_ms: index === 0 ? 3600000 : 0, categories: index === 0 ? [{ category: '游戏', total_ms: 3600000 }] : [] })),
  intervals: [{ state: 'active', app_id: app.id, category: '游戏', start_ms: start, end_ms: start + 3600000 }, { state: 'unknown', start_ms: start + 3600000, end_ms: start + 7200000 }],
};

describe('usage statistics navigation', () => {
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
    fireEvent.click(screen.getByRole('button', { name: 'usage.changeAppCategory' }));
    expect(selectApp).not.toHaveBeenCalled();
    expect(screen.getByRole('combobox')).toHaveValue('auto');
    fireEvent.change(screen.getByRole('combobox'), { target: { value: '游戏' } });
    fireEvent.click(screen.getByRole('button', { name: 'common.save' }));
    expect(saveCategory).toHaveBeenCalledWith(app.id, '游戏', 7);
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
  });

  it('restores automatic classification and keeps a failed change open for retry', async () => {
    const manual = { ...app, category_is_manual: true };
    const saveCategory = vi.fn().mockRejectedValueOnce(new Error('USAGE_SOURCE_CHANGED')).mockResolvedValue(undefined);
    render(<UsageStatisticsView date={date} today={date} day={{ ...day, apps: [manual] }} selectedApp={manual}
      loading={false} selectDate={vi.fn()} selectApp={vi.fn()} clearApp={vi.fn()} refresh={vi.fn()} saveCategory={saveCategory} />);
    fireEvent.click(screen.getByRole('button', { name: 'usage.changeAppCategory' }));
    expect(screen.getByRole('combobox')).toHaveValue('游戏');
    fireEvent.change(screen.getByRole('combobox'), { target: { value: 'auto' } });
    fireEvent.click(screen.getByRole('button', { name: 'common.save' }));
    await waitFor(() => expect(screen.getByText('usage.errors.USAGE_SOURCE_CHANGED')).toBeInTheDocument());
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'common.save' }));
    expect(saveCategory).toHaveBeenLastCalledWith(app.id, null, 7);
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
  });

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
