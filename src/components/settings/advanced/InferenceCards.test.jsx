import React from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import { BackgroundSchedulerCard, ClipBackendCard } from './InferenceCards';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key, options) => {
      if (key.endsWith('ann_failure_detail')) {
        return `${options.count} · ${options.code} · ${options.retryAt}`;
      }
      return key;
    },
  }),
}));

describe('ClipBackendCard ANN health', () => {
  it('shows circuit state without claiming image search is unavailable', () => {
    const onRetryAnn = vi.fn();
    render(
      <ClipBackendCard
        status={{
          clip_backend: {
            index_backlog: 0,
            index_stalled: 0,
            failure_count: 0,
            ann_state: 'armed',
            ann_generation: 7,
            ann_build_state: 'circuit_open',
            ann_build_failure_count: 3,
            ann_build_error_code: 'builder_missing',
            ann_build_next_retry_at: '2026-08-15T00:00:00Z',
            ann_last_error: 'carbonpaper-ml.exe was not found',
          },
        }}
        statusLoading={false}
        onRefresh={vi.fn()}
        onRunIndexNow={vi.fn()}
        onStopIndexNow={vi.fn()}
        indexRunning={false}
        indexStopping={false}
        indexProgress={null}
        indexRun={null}
        backfill={null}
        backfillBusy={false}
        onBackfillDecision={vi.fn()}
        onRetryAnn={onRetryAnn}
        annRetrying={false}
      />,
    );

    expect(screen.getByText('settings.advanced.clip_backend.ann_circuit_open')).toBeInTheDocument();
    expect(screen.getByText('settings.advanced.clip_backend.ann_search_still_available')).toBeInTheDocument();
    expect(screen.getByText('settings.advanced.clip_backend.ann_ready')).toBeInTheDocument();
    expect(screen.getByText(/3 · builder_missing/)).toBeInTheDocument();

    fireEvent.click(screen.getByText('settings.advanced.clip_backend.ann_retry_now'));
    expect(onRetryAnn).toHaveBeenCalledTimes(1);
  });
});

describe('BackgroundSchedulerCard status', () => {
  it.each([
    ['disabled', 'settings.advanced.background_processing.states.disabled'],
    ['clustering_already_running', 'settings.advanced.background_processing.states.organizing'],
    ['waiting_for_index', 'settings.advanced.background_processing.states.preparing'],
  ])('shows a user-facing label for %s', (blockedReason, expectedKey) => {
    render(
      <BackgroundSchedulerCard
        enabled
        saving={false}
        status={{ blocked_reason: blockedReason }}
        onChange={vi.fn()}
        onRefresh={vi.fn()}
      />,
    );

    expect(screen.getByText(expectedKey)).toBeInTheDocument();
  });
});
