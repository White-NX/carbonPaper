import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useCriticalErrors } from './useCriticalErrors';

vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));

describe('useCriticalErrors', () => {
  let emit;
  beforeEach(() => {
    emit = null;
    listen.mockImplementation(async (name, callback) => {
      if (name === 'critical-error') emit = callback;
      return vi.fn();
    });
  });

  it('shows errors reported before the window existed, each one once', async () => {
    invoke.mockImplementation(async (command) => {
      if (command === 'get_critical_errors') {
        // Reported while the record was being read, so it arrives both ways.
        emit({ payload: { id: 2, message: 'second' } });
        return [{ id: 1, message: 'first' }, { id: 2, message: 'second' }];
      }
      if (command === 'get_log_dir') return 'C:\\logs';
      return null;
    });

    const { result } = renderHook(() => useCriticalErrors());
    await waitFor(() => expect(result.current.criticalErrors).toEqual(['first', 'second']));
    expect(listen.mock.invocationCallOrder[0]).toBeLessThan(invoke.mock.invocationCallOrder[0]);

    act(() => emit({ payload: { id: 3, message: 'third' } }));
    expect(result.current.criticalErrors).toEqual(['first', 'second', 'third']);
    await waitFor(() => expect(result.current.criticalErrorLogPath).toBe('C:\\logs'));
  });

  it('stays empty when nothing has gone wrong', async () => {
    invoke.mockResolvedValue([]);

    const { result } = renderHook(() => useCriticalErrors());
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('get_critical_errors'));
    await act(async () => {});
    expect(result.current.criticalErrors).toEqual([]);
    expect(invoke).not.toHaveBeenCalledWith('get_log_dir');
  });
});
