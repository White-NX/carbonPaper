import React from 'react';
import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../../lib/auth_api';
import { SettingsPageContext } from '../SettingsActivityContext';
import { useSmartClusterControls } from './useSmartClusterControls';

vi.mock('../../../lib/auth_api', () => ({ withAuth: vi.fn((action) => action()) }));
vi.mock('../../../hooks/useTauriEventListener', () => ({ useTauriEventListener: vi.fn() }));
vi.mock('../../../lib/settings_api', () => ({ notifySettingsChanged: vi.fn(async () => {}) }));

const status = { pending_count: 3, enabled_cluster_count: 2, total_cluster_count: 4 };
const statusCalls = () => invoke.mock.calls.filter(([command]) => command === 'smart_cluster_status');

describe('smart cluster settings polling', () => {
  let active;
  function Page({ children }) {
    return <SettingsPageContext.Provider value={active}>{children}</SettingsPageContext.Provider>;
  }

  beforeEach(() => {
    vi.useFakeTimers();
    active = true;
    invoke.mockImplementation(async (command) => {
      if (command === 'check_model_files') return { 'bge-reranker-v2-m3': { complete: true } };
      if (command === 'smart_cluster_status') return status;
      return undefined;
    });
  });
  afterEach(() => { vi.useRealTimers(); });

  it('waits for a slow request before scheduling another poll', async () => {
    let resolveStatus;
    invoke.mockImplementation((command) => command === 'smart_cluster_status'
      ? new Promise((done) => { resolveStatus = done; })
      : Promise.resolve({}));
    const { result, unmount } = renderHook(() => useSmartClusterControls(), { wrapper: Page });
    await act(async () => { await vi.advanceTimersByTimeAsync(30000); });
    expect(statusCalls()).toHaveLength(1);
    expect(result.current.scStatus).toBeNull();
    await act(async () => resolveStatus(status));
    expect(result.current.scStatus).toEqual(status);
    await act(async () => { await vi.advanceTimersByTimeAsync(9999); });
    expect(statusCalls()).toHaveLength(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(1); });
    expect(statusCalls()).toHaveLength(2);
    unmount();
    await act(async () => resolveStatus(status));
    await act(async () => { await vi.advanceTimersByTimeAsync(30000); });
    expect(statusCalls()).toHaveLength(2);
  });

  it('shares an in-flight request when leaving and reopening the page', async () => {
    let resolveStatus;
    invoke.mockImplementation((command) => command === 'smart_cluster_status'
      ? new Promise((done) => { resolveStatus = done; })
      : Promise.resolve({}));
    const { rerender, unmount } = renderHook(() => useSmartClusterControls(), { wrapper: Page });
    active = false;
    rerender();
    await act(async () => { await vi.advanceTimersByTimeAsync(30000); });
    active = true;
    rerender();
    expect(statusCalls()).toHaveLength(1);
    await act(async () => resolveStatus(status));
    await act(async () => { await vi.advanceTimersByTimeAsync(10000); });
    expect(statusCalls()).toHaveLength(2);
    unmount();
    await act(async () => resolveStatus(status));
  });

  it('retries failed reads without prompting and stops polling on inactive pages', async () => {
    invoke.mockRejectedValueOnce(new Error('model probe unavailable'));
    invoke.mockRejectedValueOnce(new Error('AUTH_REQUIRED'));
    const { result, rerender } = renderHook(() => useSmartClusterControls(), { wrapper: Page });
    await act(async () => {});
    expect(result.current.scStatus).toBeNull();
    await act(async () => { await vi.advanceTimersByTimeAsync(10000); });
    expect(result.current.scStatus).toEqual(status);
    expect(withAuth).toHaveBeenLastCalledWith(expect.any(Function));
    active = false;
    rerender();
    await act(async () => { await vi.advanceTimersByTimeAsync(30000); });
    expect(statusCalls()).toHaveLength(2);
  });
});
