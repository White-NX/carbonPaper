import React from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import { BackgroundSchedulerCard, ClipBackendCard, SemanticBackendCard } from './InferenceCards';

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

describe('manual index phases', () => {
  const baseStatus = {
    backend: { index_backlog: 3, index_stalled: 0, indexed_vectors: 4, failure_count: 1 },
  };

  it('does not present retry_wait as a running job', () => {
    const onRun = vi.fn();
    const onStop = vi.fn();
    render(
      <SemanticBackendCard
        status={baseStatus}
        statusLoading={false}
        onRefresh={vi.fn()}
        onRunIndexNow={onRun}
        onStopIndexNow={onStop}
        indexRunning={false}
        indexPhase="retry_wait"
        indexRetryAt={Date.UTC(2026, 7, 26, 12, 0, 0)}
        indexStopping={false}
        indexProgress={null}
        indexRun={null}
      />,
    );

    expect(screen.getByText('settings.advanced.semantic_backend.run_retry_wait')).toBeInTheDocument();
    expect(screen.getByText('settings.advanced.semantic_backend.run_stop')).toBeInTheDocument();
    fireEvent.click(screen.getByText('settings.advanced.semantic_backend.run_retry'));
    expect(onRun).toHaveBeenCalledTimes(1);
    expect(onStop).not.toHaveBeenCalled();
  });

  it('keeps queued work stoppable', () => {
    const onStop = vi.fn();
    render(
      <SemanticBackendCard
        status={baseStatus}
        statusLoading={false}
        onRefresh={vi.fn()}
        onRunIndexNow={vi.fn()}
        onStopIndexNow={onStop}
        indexRunning={true}
        indexPhase="queued"
        indexRetryAt={null}
        indexStopping={false}
        indexProgress={null}
        indexRun={null}
      />,
    );

    fireEvent.click(screen.getByText('settings.advanced.semantic_backend.run_stop'));
    expect(onStop).toHaveBeenCalledTimes(1);
  });

  it('shows stopping feedback for a queued task', () => {
    render(<ClipBackendCard status={{ clip_backend: {} }} indexPhase="queued" indexStopping />);
    expect(screen.getByText('settings.advanced.clip_backend.run_stopping')).toBeInTheDocument();
    expect(screen.getByText('settings.advanced.clip_backend.run_stop_pending')).toBeDisabled();
    expect(screen.queryByText('settings.advanced.clip_backend.run_queued')).not.toBeInTheDocument();
  });

  it.each([
    ['waiting_for_unlock', 'run_unlock'],
    ['waiting_for_verification', 'run_verify'],
  ])('preserves progress during %s and offers resume and stop', (phase, action) => {
    const resume = vi.fn();
    const stop = vi.fn();
    render(<ClipBackendCard status={{ clip_backend: {} }} indexPhase={phase}
      indexProgress={{ processed: 4, total: 10 }} onRunIndexNow={resume} onStopIndexNow={stop} />);
    expect(screen.getByRole('progressbar')).toHaveAttribute('value', '0.4');
    expect(screen.getByText(`settings.advanced.clip_backend.run_${phase}`)).toBeInTheDocument();
    if (phase === 'waiting_for_verification') {
      expect(screen.queryByText('settings.advanced.clip_backend.run_unlock')).not.toBeInTheDocument();
    }
    fireEvent.click(screen.getByText(`settings.advanced.clip_backend.${action}`));
    fireEvent.click(screen.getByText('settings.advanced.clip_backend.run_stop'));
    expect(resume).toHaveBeenCalledOnce();
    expect(stop).toHaveBeenCalledOnce();
  });

  it('does not keep showing a stale queued summary after the scheduler finishes', () => {
    render(
      <SemanticBackendCard
        status={baseStatus}
        statusLoading={false}
        onRefresh={vi.fn()}
        onRunIndexNow={vi.fn()}
        onStopIndexNow={vi.fn()}
        indexRunning={false}
        indexPhase="idle"
        indexRetryAt={null}
        indexStopping={false}
        indexProgress={null}
        indexRun={{ queued: true }}
      />,
    );

    expect(screen.queryByText('settings.advanced.semantic_backend.run_queued')).not.toBeInTheDocument();
    expect(screen.getByText('settings.advanced.semantic_backend.run_now_hint')).toBeInTheDocument();
  });
});
