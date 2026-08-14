import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';

import { getClipBackfillOffer } from '../../lib/task_api';
import { useAdvancedSectionController } from './useAdvancedSectionController';

vi.mock('../../lib/auth_api', () => ({
  withAuth: vi.fn((fn) => fn()),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => vi.fn()),
}));

vi.mock('../../lib/task_api', () => ({
  getClipBackfillOffer: vi.fn(),
  setClipBackfillDecision: vi.fn(),
}));

const t = (key) => key;

describe('useAdvancedSectionController', () => {
  let clipRunActive;

  beforeEach(() => {
    vi.clearAllMocks();
    clipRunActive = true;
    getClipBackfillOffer.mockResolvedValue({
      migration_settled: true,
      decision: null,
      should_ask: false,
      never_indexed: 0,
      stalled: 0,
    });
    invoke.mockImplementation(async (command) => {
      if (command === 'get_advanced_config') return {};
      if (command === 'storage_is_startup_vacuum_in_progress') return false;
      if (command === 'get_rust_ocr_model_status') return {};
      if (command === 'get_ml_ocr_status') return {};
      if (command === 'get_ml_semantic_status') {
        return {
          backend: { index_run_active: false },
          clip_backend: { index_run_active: clipRunActive },
        };
      }
      if (command === 'clip_index_stop_now') return true;
      return undefined;
    });
  });

  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  it('restores and can stop a CLIP run owned by an earlier settings mount', async () => {
    const first = renderHook(() => useAdvancedSectionController({
      monitorStatus: 'stopped',
      t,
    }));

    await waitFor(() => expect(first.result.current.clipIndexRunning).toBe(true));
    first.unmount();

    const reopened = renderHook(() => useAdvancedSectionController({
      monitorStatus: 'stopped',
      t,
    }));

    await waitFor(() => expect(reopened.result.current.clipIndexRunning).toBe(true));

    await act(async () => {
      await reopened.result.current.handleStopClipIndex();
    });
    expect(invoke).toHaveBeenCalledWith('clip_index_stop_now');
    expect(reopened.result.current.clipIndexStopping).toBe(true);

    clipRunActive = false;
    await act(async () => {
      await reopened.result.current.refreshSemanticStatus();
    });

    expect(reopened.result.current.clipIndexRunning).toBe(false);
    expect(reopened.result.current.clipIndexStopping).toBe(false);
    expect(invoke).toHaveBeenCalledWith('storage_is_startup_vacuum_in_progress');
    expect(invoke).not.toHaveBeenCalledWith('storage_get_startup_vacuum_status');
    expect(invoke).toHaveBeenCalledWith('get_ml_semantic_status', {
      refreshDiagnostics: true,
    });
    expect(getClipBackfillOffer).toHaveBeenCalledWith(true);
  });

  it('waits for each OCR status request before scheduling the next poll', async () => {
    vi.useFakeTimers();
    clipRunActive = false;
    const ocrResolvers = [];
    invoke.mockImplementation((command) => {
      if (command === 'get_advanced_config') return Promise.resolve({});
      if (command === 'storage_is_startup_vacuum_in_progress') return Promise.resolve(false);
      if (command === 'get_rust_ocr_model_status') return Promise.resolve({});
      if (command === 'get_ml_ocr_status') {
        return new Promise((resolve) => ocrResolvers.push(resolve));
      }
      if (command === 'get_ml_semantic_status') {
        return Promise.resolve({
          backend: { index_run_active: false },
          clip_backend: { index_run_active: false },
        });
      }
      return Promise.resolve(undefined);
    });

    const hook = renderHook(() => useAdvancedSectionController({
      monitorStatus: 'stopped',
      t,
    }));

    await act(async () => Promise.resolve());
    expect(invoke.mock.calls.filter(([command]) => command === 'get_ml_ocr_status')).toHaveLength(1);

    await act(async () => {
      vi.advanceTimersByTime(15000);
      await Promise.resolve();
    });
    expect(invoke.mock.calls.filter(([command]) => command === 'get_ml_ocr_status')).toHaveLength(1);

    await act(async () => {
      ocrResolvers[0]({ state: 'stopped' });
      await Promise.resolve();
    });
    await act(async () => {
      vi.advanceTimersByTime(5000);
      await Promise.resolve();
    });
    expect(invoke.mock.calls.filter(([command]) => command === 'get_ml_ocr_status')).toHaveLength(2);

    hook.unmount();
  });

  it('persists the Python classification rollback and marks it for restart', async () => {
    clipRunActive = false;
    const hook = renderHook(() => useAdvancedSectionController({
      monitorStatus: 'running',
      t,
    }));

    await waitFor(() => expect(hook.result.current.config).toEqual({}));

    await act(async () => {
      await hook.result.current.handleToggleRustClassification(false);
    });

    expect(invoke).toHaveBeenCalledWith('set_advanced_config', {
      config: { classification_runtime: 'python' },
    });
    expect(hook.result.current.config.classification_runtime).toBe('python');
    expect(hook.result.current.classificationRuntimeChanged).toBe(true);
  });

  it('runs an authenticated manual ANN retry and refreshes diagnostics', async () => {
    clipRunActive = false;
    invoke.mockImplementation(async (command) => {
      if (command === 'get_advanced_config') return {};
      if (command === 'storage_is_startup_vacuum_in_progress') return false;
      if (command === 'get_rust_ocr_model_status') return {};
      if (command === 'get_ml_ocr_status') return {};
      if (command === 'clip_ann_retry_now') return true;
      if (command === 'get_ml_semantic_status') {
        return {
          backend: { index_run_active: false },
          clip_backend: { index_run_active: false, ann_build_state: 'healthy' },
        };
      }
      return undefined;
    });
    const hook = renderHook(() => useAdvancedSectionController({
      monitorStatus: 'stopped',
      t,
    }));

    await waitFor(() => expect(hook.result.current.config).toEqual({}));
    await act(async () => {
      await hook.result.current.handleRetryClipAnn();
    });

    expect(invoke).toHaveBeenCalledWith('clip_ann_retry_now');
    expect(invoke).toHaveBeenCalledWith('get_ml_semantic_status', {
      refreshDiagnostics: true,
    });
    expect(hook.result.current.clipAnnRetrying).toBe(false);
  });
});
