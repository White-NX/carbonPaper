import { act, renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { requestAuth } from '../../../lib/auth_api';
import { useIndexTaskController } from './useIndexTaskController';

vi.mock('../../../lib/auth_api', () => ({
  withAuth: vi.fn((fn) => fn()), requestAuth: vi.fn(),
}));
const t = (key) => key;

describe.each(['clip', 'semantic'])('%s index controls', (kind) => {
  beforeEach(() => { vi.clearAllMocks(); });

  it('waits for a pending enqueue before stopping it', async () => {
    let acknowledge;
    invoke.mockImplementation((command) => command.endsWith('_run_now')
      ? new Promise((resolve) => { acknowledge = resolve; }) : Promise.resolve(true));
    const queued = { phase: 'queued', running: false };
    const refresh = vi.fn().mockResolvedValueOnce({ [kind]: queued })
      .mockResolvedValueOnce({ [kind]: { phase: 'idle', running: false } });
    const hook = renderHook(() => useIndexTaskController(kind, { phase: 'idle' }, refresh, t));
    let start;
    let stop;
    act(() => { start = hook.result.current.run(); });
    act(() => { stop = hook.result.current.stop(); });
    expect(hook.result.current.stopping).toBe(true);
    expect(invoke).not.toHaveBeenCalledWith(`${kind}_index_stop_now`);
    await act(async () => { acknowledge({ queued: true }); await start; await stop; });
    expect(invoke).toHaveBeenCalledWith(`${kind}_index_stop_now`);
    expect(hook.result.current.stopping).toBe(false);
  });

  it.each(['waiting_for_unlock', 'waiting_for_verification'])('resumes %s without replacing the request or losing progress', async (phase) => {
    requestAuth.mockResolvedValue(true);
    const progress = { phase, processed: 4, total: 10 };
    const refresh = vi.fn(async () => ({ [kind]: { ...progress, phase: 'running' } }));
    const hook = renderHook(() => useIndexTaskController(kind, progress, refresh, t));
    await act(async () => { await hook.result.current.run(); });
    expect(requestAuth).toHaveBeenCalledOnce();
    expect(invoke).not.toHaveBeenCalledWith(`${kind}_index_run_now`);
    expect(hook.result.current.progress.processed).toBe(4);
    expect(refresh).toHaveBeenCalledWith({ fresh: true });
  });

  it('shows a stop failure and keeps the task available for another stop attempt', async () => {
    const warning = vi.spyOn(console, 'warn').mockImplementation(() => {});
    invoke.mockRejectedValueOnce(new Error('WINDOW_NOT_AUTHORIZED'));
    const hook = renderHook(() => useIndexTaskController(kind, { phase: 'running' }, vi.fn(), t));
    try {
      await act(async () => { await hook.result.current.stop(); });
      expect(hook.result.current.error).toBe(`settings.advanced.${kind}_backend.run_stop_failed`);
      expect(hook.result.current.stopping).toBe(false);
      expect(hook.result.current.running).toBe(true);
    } finally { warning.mockRestore(); }
  });
});
