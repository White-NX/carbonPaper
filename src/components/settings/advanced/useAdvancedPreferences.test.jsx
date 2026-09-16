import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { useAdvancedPreferences } from './useAdvancedPreferences';

vi.mock('../../../hooks/useTauriEventListener', () => ({ useTauriEventListener: () => {} }));
vi.mock('../../../lib/auth_api', () => ({ withAuth: (call) => call() }));
const config = { cpu_limit_enabled: true, cpu_limit_percent: 10, use_dml: true, dml_device_id: 9, ocr_timeout_secs: 120 };
let fail;
beforeEach(() => {
  fail = false;
  invoke.mockReset().mockImplementation(async (command) => {
    if (command === 'get_advanced_config') return { ...config };
    if (command === 'enumerate_gpus') return [{ id: 0, name: 'GPU' }];
    if (command === 'set_advanced_config' && fail) throw new Error('disk full');
    return null;
  });
});
const t = (key) => key;

describe('advanced preference writes', () => {
  it('does not write a different GPU while reading unavailable hardware', async () => {
    const { result } = renderHook(() => useAdvancedPreferences({ monitorStatus: 'running', t }));
    await waitFor(() => expect(result.current.gpus).toHaveLength(1));
    expect(invoke.mock.calls.some(([command]) => command === 'set_advanced_config')).toBe(false);
    expect(result.current.config.dml_device_id).toBe(9);
  });
  it('writes only the edited field and flags restart only after success', async () => {
    const { result } = renderHook(() => useAdvancedPreferences({ monitorStatus: 'running', t }));
    await waitFor(() => expect(result.current.loading).toBe(false));
    fail = true;
    await act(() => result.current.handleCpuPercentChange(20));
    expect(result.current.config.cpu_limit_percent).toBe(10);
    expect(result.current.cpuChanged).toBe(false);
    fail = false;
    await act(() => result.current.handleCpuPercentChange(20));
    expect(invoke).toHaveBeenCalledWith('set_advanced_config', { config: { cpu_limit_percent: 20 } });
    expect(result.current.config.cpu_limit_percent).toBe(20);
    expect(result.current.cpuChanged).toBe(true);
  });
});
