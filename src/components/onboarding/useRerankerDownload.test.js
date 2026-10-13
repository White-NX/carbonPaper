import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { useRerankerDownload } from './useRerankerDownload';

vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => vi.fn()) }));
beforeEach(() => { localStorage.clear(); invoke.mockReset(); });

describe('resumed smart cluster setup', () => {
  it.each(['download_model', 'set_advanced_config'])('waits for unlock when %s loses authentication instead of launching Hello', async (blockedCommand) => {
    localStorage.setItem('onboarding.pendingSmartCluster', '1');
    const notify = vi.fn();
    let denied = false;
    invoke.mockImplementation(async (command) => {
      if (command === blockedCommand && !denied) { denied = true; throw new Error('AUTH_REQUIRED'); }
      return true;
    });
    const { result, rerender } = renderHook(({ ready }) => useRerankerDownload({ ready, notify, t: (key) => key }), { initialProps: { ready: true } });
    await waitFor(() => expect(denied).toBe(true));
    await waitFor(() => expect(result.current.status).toBe('queued'));
    expect(localStorage.getItem('onboarding.pendingSmartCluster')).toBe('1');
    expect(notify).not.toHaveBeenCalled();
    expect(invoke.mock.calls.some(([command]) => command.startsWith('credential_'))).toBe(false);
    await act(async () => {});
    expect(invoke.mock.calls.filter(([command]) => command === blockedCommand)).toHaveLength(1);
    rerender({ ready: false });
    rerender({ ready: true });
    await waitFor(() => expect(result.current.status).toBe('done'));
    expect(localStorage.getItem('onboarding.pendingSmartCluster')).toBeNull();
    expect(invoke.mock.calls.some(([command]) => command.startsWith('credential_'))).toBe(false);
  });
});
