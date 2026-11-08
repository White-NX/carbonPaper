import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { listen } from '@tauri-apps/api/event';
import { takeTrayNavigation } from '../lib/tray_api';
import { useTrayNavigation } from './useTrayNavigation';

vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));
vi.mock('../lib/tray_api', () => ({ takeTrayNavigation: vi.fn() }));

beforeEach(() => {
  listen.mockReset().mockResolvedValue(vi.fn());
  takeTrayNavigation.mockReset().mockResolvedValue(null);
});

describe('tray handoff', () => {
  it('receives a stored request after a cold WebView becomes ready and unlocked', async () => {
    const onNavigate = vi.fn();
    const intent = { kind: 'snapshot', id: 4, timestamp_ms: 1700000000123 };
    takeTrayNavigation.mockResolvedValueOnce(intent);
    const { rerender } = renderHook(({ enabled }) => useTrayNavigation({ enabled, onNavigate }), { initialProps: { enabled: false } });
    expect(takeTrayNavigation).not.toHaveBeenCalled();
    rerender({ enabled: true });
    await waitFor(() => expect(onNavigate).toHaveBeenCalledWith(intent));
    expect(listen).toHaveBeenCalledWith('tray-navigate', expect.any(Function));
  });

  it('discards an IPC result if the interface locks before it arrives', async () => {
    let resolve;
    takeTrayNavigation.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    const onNavigate = vi.fn();
    const { rerender } = renderHook(({ enabled }) => useTrayNavigation({ enabled, onNavigate }), { initialProps: { enabled: true } });
    await waitFor(() => expect(takeTrayNavigation).toHaveBeenCalled());
    rerender({ enabled: false });
    await act(async () => resolve({ kind: 'search', query: 'private notes' }));
    expect(onNavigate).not.toHaveBeenCalled();
  });

  it('drains a second intent notified while the first read is still in flight', async () => {
    let resolve;
    takeTrayNavigation.mockImplementationOnce(() => new Promise((done) => { resolve = done; }))
      .mockResolvedValueOnce({ kind: 'search', query: 'meeting' });
    const onNavigate = vi.fn();
    renderHook(() => useTrayNavigation({ enabled: true, onNavigate }));
    await waitFor(() => expect(takeTrayNavigation).toHaveBeenCalledTimes(1));
    await act(async () => {
      void listen.mock.calls[0][1]();
      resolve({ kind: 'timeline' });
    });
    await waitFor(() => expect(onNavigate).toHaveBeenCalledTimes(2));
    expect(onNavigate.mock.calls.map(([intent]) => intent.kind)).toEqual(['timeline', 'search']);
  });
});
