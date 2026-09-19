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

const t = (key) => key;

beforeEach(() => {
  vi.spyOn(console, 'error').mockImplementation(() => {});
  invoke.mockReset();
  invoke.mockImplementation(async (command) => {
    if (command === 'get_advanced_config') return { classification_enabled: true, smart_cluster_enabled: false };
    throw new Error(`Unexpected command: ${command}`);
  });
});

afterEach(() => vi.restoreAllMocks());

async function renderController() {
  const hook = renderHook(() => useFeaturesController({
    t,
    featureModeDefinitions: [{ value: 'minimal', config: {} }],
    getFeatureMode: () => 'minimal',
  }));
  await waitFor(() => expect(hook.result.current.loading).toBe(false));
  return hook;
}

describe('useFeaturesController', () => {
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

});
