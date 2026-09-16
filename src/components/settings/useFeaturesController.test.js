import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { useFeaturesController } from './useFeaturesController';

vi.mock('./organize/useModelInventory', () => ({
  useModelInventory: () => ({}),
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('./organize/useSmartClusterControls', () => ({
  useSmartClusterControls: () => ({ scModelAvailable: true }),
}));

const firstTime = Date.parse('2026-09-14T10:00:00Z');
const retryTime = firstTime + 60_000;
const t = (key) => key;
let replies;
let progress;

beforeEach(() => {
  vi.spyOn(Date, 'now').mockReturnValue(firstTime);
  vi.spyOn(console, 'error').mockImplementation(() => {});
  replies = [{ error: '' }];
  progress = null;
  invoke.mockReset();
  // Keep the real API wrapper and status hook so body errors follow the full
  // frontend path from invoke() through the controller's retry handling.
  invoke.mockImplementation(async (command, payload) => {
    if (command === 'get_advanced_config') return { clustering_enabled: true };
    if (command === 'monitor_get_clustering_status') {
      return {
        status: 'success',
        config: {},
        scheduler: { tasks: [] },
        clustering_progress: progress,
      };
    }
    if (command === 'monitor_run_clustering') {
      const reply = replies.length > 1 ? replies.shift() : replies[0];
      progress = {
        manual: true,
        active: false,
        phase: 'error' in reply
          ? 'interrupted'
          : reply.status === 'needs_user_choice' ? 'awaiting_choice' : 'results_ready',
        start_time: payload.startTime,
        end_time: payload.endTime,
      };
      return reply;
    }
    throw new Error(`Unexpected command: ${command}`);
  });
});

afterEach(() => vi.restoreAllMocks());

async function renderController() {
  const hook = renderHook(() => useFeaturesController({
    monitorStatus: 'running',
    t,
    featureModeDefinitions: [{ value: 'minimal', config: {} }],
    getFeatureMode: () => 'minimal',
  }));
  await waitFor(() => expect(hook.result.current.loading).toBe(false));
  act(() => hook.result.current.setRangeStart('2026-08-01'));
  return hook;
}

function clusteringPayloads() {
  return invoke.mock.calls
    .filter(([command]) => command === 'monitor_run_clustering')
    .map(([, payload]) => payload);
}

describe('useFeaturesController clustering retries', () => {
  it('saves background timing as an authenticated partial preference and retains it on failure', async () => {
    const existing = invoke.getMockImplementation();
    let rejectSave = false;
    invoke.mockImplementation(async (command, payload) => {
      if (command === 'set_advanced_config') {
        if (rejectSave) throw new Error('disk unavailable');
        return null;
      }
      return existing(command, payload);
    });
    const { result } = await renderController();
    await act(async () => result.current.handleBackgroundTimingChange('idle_only'));
    expect(invoke).toHaveBeenCalledWith('set_advanced_config', {
      config: { background_scheduling_mode: 'idle_only' },
    });
    expect(result.current.config.background_scheduling_mode).toBe('idle_only');
    rejectSave = true;
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    await act(async () => result.current.handleBackgroundTimingChange('auto'));
    expect(result.current.config.background_scheduling_mode).toBe('idle_only');
    expect(result.current.backgroundTimingError).toBe(true);
    expect(result.current.backgroundTimingSaving).toBe(false);
  });

  it.each([
    ['', 'tasks.clusteringFailed'],
    ['Python clustering failed', 'Python clustering failed'],
  ])('shows a body error and retains the open-ended range on retry: %j', async (error, expected) => {
    replies = [{ error }];
    const { result } = await renderController();
    await act(async () => result.current.handleRunClustering());
    expect(result.current.clusteringError).toBe(expected);
    expect(result.current.clusteringNotice).toBeNull();
    expect(result.current.clusteringProgress.phase).toBe('interrupted');
    expect(result.current.clusteringRunning).toBe(false);

    Date.now.mockReturnValue(retryTime);
    await act(async () => result.current.handleRunClustering());
    const payloads = clusteringPayloads();
    expect(payloads).toHaveLength(2);
    expect(payloads[0].endTime).toBe(firstTime / 1000);
    expect(payloads[1]).toEqual(payloads[0]);
    expect(result.current.clusteringError).toBe(expected);
  });

  it('preserves the range when an empty error follows a resource choice', async () => {
    replies = [
      { status: 'needs_user_choice', reason: 'low_memory', estimate: { count: 1000 } },
      { error: '' },
    ];
    const { result } = await renderController();
    let pending;
    await act(async () => { pending = result.current.handleRunClustering(); });
    expect(result.current.clusteringResourceChoice).not.toBeNull();
    await act(async () => {
      result.current.resolveClusteringResourceChoice(true);
      await pending;
    });
    expect(result.current.clusteringError).toBe('tasks.clusteringFailed');

    Date.now.mockReturnValue(retryTime);
    await act(async () => result.current.handleRunClustering());
    const payloads = clusteringPayloads();
    expect(payloads.map((payload) => payload.endTime)).toEqual([
      firstTime / 1000, firstTime / 1000, firstTime / 1000,
    ]);
    expect(payloads[1].clusteringMode).toBe('batched');
  });

  it('starts a fresh range after a successful retry', async () => {
    replies = [{ error: '' }, { status: 'success' }];
    const { result } = await renderController();
    await act(async () => result.current.handleRunClustering());
    Date.now.mockReturnValue(retryTime);
    await act(async () => result.current.handleRunClustering());
    expect(result.current.clusteringError).toBeNull();
    expect(result.current.clusteringNotice).toBe('settings.features.management.clustering.progress.completed');

    const nextTime = retryTime + 60_000;
    Date.now.mockReturnValue(nextTime);
    await act(async () => result.current.handleRunClustering());
    expect(clusteringPayloads().map((payload) => payload.endTime)).toEqual([
      firstTime / 1000, firstTime / 1000, nextTime / 1000,
    ]);
  });
});
