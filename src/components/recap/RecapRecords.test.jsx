import React from 'react';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { getRecapRecords } from '../../lib/recap_api';
import { RecordList } from './RecapSources';
import RecapRecords from './RecapRecords';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));
vi.mock('../../lib/recap_api', () => ({ getRecapRecords: vi.fn(), recapErrorKey: () => 'error' }));
const items = Array.from({ length: 10000 }, (_, id) => ({ id, timestamp_ms: id, process_name: 'Editor', window_title: `Record ${id}` }));
const action = { open: vi.fn(), floating: true };
const time = (ms) => String(ms);
const batches = [{ start_ms: 1, end_ms: 10000, updated_at_ms: 1, record_count: 10000 }];
beforeEach(() => {
  getRecapRecords.mockReset().mockImplementation(async (_date, _period, cursor) => {
    const start = Number(cursor || 0);
    return { items: items.slice(start, start + 100), total: items.length, next_cursor: start + 100 < items.length ? String(start + 100) : null };
  });
});

describe('bounded record browsing', () => {
  it('mounts only viewport rows for ten thousand records, even after a deep scroll', () => {
    render(<RecordList items={items} time={time} action={action} label="records" />);
    expect(screen.getAllByRole('listitem').length).toBeLessThanOrEqual(26);
    const list = screen.getByRole('list');
    list.scrollTop = 48000;
    fireEvent.scroll(list);
    expect(screen.getAllByRole('listitem').length).toBeLessThanOrEqual(26);
    expect(screen.getByText('Record 1000')).toBeInTheDocument();
    expect(screen.queryByText('Record 0')).not.toBeInTheDocument();
  });
  it('fetches subsequent pages as the viewport reaches them', async () => {
    render(<RecapRecords date="2026-10-01" batches={batches} active time={time} action={action} period="" onPeriod={vi.fn()} position={{ current: 0 }} />);
    await screen.findByText('Record 0');
    const list = screen.getByRole('list');
    list.scrollTop = 96 * 48;
    fireEvent.scroll(list);
    await waitFor(() => expect(getRecapRecords).toHaveBeenCalledWith('2026-10-01', null, '100'));
    expect(await screen.findByText('Record 100')).toBeInTheDocument();
    expect(screen.getAllByRole('listitem').length).toBeLessThanOrEqual(26);
  });
  it('ignores a late page from a previous date', async () => {
    let resolve;
    getRecapRecords.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    const props = { batches, active: true, time, action, period: '', onPeriod: vi.fn(), position: { current: 0 } };
    const { rerender } = render(<RecapRecords {...props} date="2026-10-01" />);
    rerender(<RecapRecords {...props} date="2026-10-02" />);
    await screen.findByText('Record 0');
    await act(async () => resolve({ items: [{ ...items[0], window_title: 'Stale secret' }], total: 1, next_cursor: null }));
    expect(screen.queryByText('Stale secret')).not.toBeInTheDocument();
  });
  it('restarts pagination after a stale cursor and keeps an explicit retry for repeated failures', async () => {
    getRecapRecords.mockRejectedValueOnce(new Error('RECAP_SOURCE_CHANGED')).mockRejectedValueOnce(new Error('RECAP_SOURCE_CHANGED'));
    render(<RecapRecords date="2026-10-01" batches={batches} active time={time} action={action} period="" onPeriod={vi.fn()} position={{ current: 0 }} />);
    await screen.findByRole('alert');
    expect(getRecapRecords).toHaveBeenCalledTimes(2);
    fireEvent.click(screen.getByRole('button', { name: 'common.retry' }));
    expect(await screen.findByText('Record 0')).toBeInTheDocument();
  });
  it('clears records on authentication failure and retries from the first page', async () => {
    render(<RecapRecords date="2026-10-01" batches={batches} active time={time} action={action} period="" onPeriod={vi.fn()} position={{ current: 0 }} />);
    await screen.findByText('Record 0');
    getRecapRecords.mockRejectedValueOnce(new Error('AUTH_REQUIRED'));
    const list = screen.getByRole('list');
    list.scrollTop = 96 * 48;
    fireEvent.scroll(list);
    await screen.findByRole('alert');
    expect(screen.queryByRole('listitem')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'common.retry' }));
    await waitFor(() => expect(getRecapRecords).toHaveBeenLastCalledWith('2026-10-01', null, null));
  });
});
