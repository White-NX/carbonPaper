import React from 'react';
import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { getSnapshotRecapContext } from '../lib/recap_api';
import DetailCard from './DetailCard';

vi.mock('react-i18next', () => ({ useTranslation: () => ({
  t: (key, options) => key === 'sidebar.recap.recordedTime' ? `${key}:${options.time}` : key,
  i18n: { language: 'en' },
}) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('../lib/monitor_api', () => ({ computeLinkScores: vi.fn(async () => []), updateScreenshotCategory: vi.fn() }));
vi.mock('../lib/recap_api', async (original) => ({ ...await original(), getSnapshotRecapContext: vi.fn() }));
vi.mock('./OfficeDocumentEntry', () => ({ default: () => null }));

const start = Date.UTC(2026, 0, 1, 0, 0);
const item = { date: '2026-01-01', calendar: { time_zone: 'Asia/Tokyo' }, activity_id: 'a1', task_id: 'task',
  task_title: 'Review capture settings', text: 'Compared event association choices.', details: [{ text: 'Keep separate activities in one window.', evidence_ids: [1] }], start_ms: start, end_ms: start + 180000 };
const event = (id) => ({ id, appName: 'Editor', windowTitle: 'Capture settings', timestamp: start + 600000 });
const advance = (ms = 120) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });
beforeEach(() => {
  vi.useFakeTimers();
  getSnapshotRecapContext.mockReset().mockResolvedValue({ status: 'ready', items: [item] });
});
afterEach(() => vi.useRealTimers());

describe('snapshot detail recap', () => {
  it('expands in place and links directly to the saved activity', async () => {
    const open = vi.fn();
    render(<DetailCard selectedEvent={event(1)} isAuthenticated onOpenRecap={open} />);
    await advance();
    const row = screen.getByRole('button', { name: item.task_title });
    expect(row).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByRole('button', { name: 'sidebar.recap.openDetails' })).not.toBeInTheDocument();
    expect(screen.getByText('sidebar.recap.recordedTime:09:00–09:03')).toBeInTheDocument();
    fireEvent.click(row);
    expect(row).toHaveAttribute('aria-expanded', 'true');
    const card = screen.getByRole('region', { name: 'sidebar.details.title' });
    expect(within(card).getByText('Capture settings')).toBeInTheDocument();
    expect(within(card).getByText(item.text)).toBeInTheDocument();
    expect(within(card).queryByText(item.details[0].text)).not.toBeInTheDocument();
    expect(screen.queryByRole('menu')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'sidebar.recap.openDetails' }));
    expect(open).toHaveBeenCalledWith({ date: item.date, task_id: item.task_id, activity_id: item.activity_id, screenshot_id: 1 });
  });

  it('preserves expansion within the same event and resets it for another event', async () => {
    getSnapshotRecapContext.mockImplementation(async (id) => ({ status: 'ready', items: [{ ...item, activity_id: `a${id}`, task_id: id === 3 ? 'other' : item.task_id }] }));
    const { rerender } = render(<DetailCard selectedEvent={event(1)} isAuthenticated />);
    await advance();
    fireEvent.click(screen.getByRole('button', { name: item.task_title }));
    rerender(<DetailCard selectedEvent={event(2)} isAuthenticated />);
    expect(screen.queryByText(item.text)).not.toBeInTheDocument();
    await advance();
    expect(screen.getByRole('button', { name: item.task_title })).toHaveAttribute('aria-expanded', 'true');
    rerender(<DetailCard selectedEvent={event(3)} isAuthenticated />);
    await advance();
    expect(screen.getByRole('button', { name: item.task_title })).toHaveAttribute('aria-expanded', 'false');
  });

  it('holds an expanded card open and resumes dismissal after collapse', async () => {
    render(<DetailCard selectedEvent={event(1)} isAuthenticated />);
    await advance();
    const row = screen.getByRole('button', { name: item.task_title });
    fireEvent.click(row);
    await advance(6000);
    expect(screen.getByRole('region', { name: 'sidebar.details.title' })).toBeInTheDocument();
    fireEvent.click(row);
    await advance(5000);
    expect(screen.queryByRole('region', { name: 'sidebar.details.title' })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'sidebar.details.title' }));
    expect(screen.getByRole('region', { name: 'sidebar.details.title' })).toBeInTheDocument();
  });

  it('holds keyboard focus open and clears recap content on lock', async () => {
    const { rerender } = render(<DetailCard selectedEvent={event(1)} isAuthenticated />);
    await advance();
    const row = screen.getByRole('button', { name: item.task_title });
    act(() => row.focus());
    await advance(6000);
    expect(screen.getByRole('region', { name: 'sidebar.details.title' })).toBeInTheDocument();
    rerender(<DetailCard selectedEvent={event(1)} isAuthenticated={false} />);
    expect(screen.queryByText(item.task_title)).not.toBeInTheDocument();
    expect(screen.queryByText(item.text)).not.toBeInTheDocument();
  });

  it('hides pending periods and unmatched records without a navigation button', async () => {
    const open = vi.fn();
    getSnapshotRecapContext.mockResolvedValueOnce({ status: 'pending', items: [] });
    const { rerender } = render(<DetailCard selectedEvent={event(1)} isAuthenticated onOpenRecap={open} />);
    await advance();
    expect(getSnapshotRecapContext).toHaveBeenLastCalledWith(1);
    expect(screen.queryByRole('region', { name: 'sidebar.recap.title' })).not.toBeInTheDocument();
    expect(screen.queryByText('sidebar.recap.pending')).not.toBeInTheDocument();
    getSnapshotRecapContext.mockResolvedValueOnce({ status: 'unmatched', items: [] });
    rerender(<DetailCard selectedEvent={event(2)} isAuthenticated onOpenRecap={open} />);
    await advance();
    expect(getSnapshotRecapContext).toHaveBeenLastCalledWith(2);
    expect(screen.queryByRole('region', { name: 'sidebar.recap.title' })).not.toBeInTheDocument();
    expect(screen.queryByText('sidebar.recap.unmatched')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'sidebar.recap.openDetails' })).not.toBeInTheDocument();
  });
});
