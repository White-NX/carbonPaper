import { renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { getIndexHealth } from '../../../lib/monitor_api';
import { useSavedCaptureFilters } from './useSavedCaptureFilters';

vi.mock('./useCaptureFilterSettings', () => ({ readSavedCaptureFilters: () => ({ processes: [], titles: [], ignoreProtected: true }) }));
beforeEach(() => { invoke.mockReset(); });

describe('background settings authentication', () => {
  it('does not launch Hello when applying saved capture filters loses the session', async () => {
    invoke.mockRejectedValue(new Error('AUTH_REQUIRED'));
    renderHook(() => useSavedCaptureFilters({ monitorStatus: 'running' }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('monitor_update_filters', expect.anything()));
    expect(invoke.mock.calls.map(([command]) => command)).toEqual(['monitor_update_filters']);
  });

  it('reports locked index health without requesting an authentication prompt', async () => {
    invoke.mockRejectedValue(new Error('AUTH_REQUIRED'));
    await expect(getIndexHealth()).rejects.toThrow('AUTH_REQUIRED');
    expect(invoke.mock.calls.map(([command]) => command)).toEqual(['storage_get_index_health']);
  });
});
