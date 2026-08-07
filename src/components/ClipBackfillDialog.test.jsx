import React from 'react';
import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    // Keys plus their interpolation, so a test can assert which number landed
    // in which sentence — the whole point of this dialog is that two counts
    // that look alike must not be reported as one.
    t: (key, vars) => (vars ? `${key}:${JSON.stringify(vars)}` : key),
  }),
}));

vi.mock('./Dialog', () => ({
  Dialog: ({ isOpen, children }) => (isOpen ? <div>{children}</div> : null),
}));

vi.mock('../lib/task_api', () => ({
  getClipBackfillOffer: vi.fn(),
  setClipBackfillDecision: vi.fn(),
}));

import ClipBackfillDialog from './ClipBackfillDialog';
import { getClipBackfillOffer, setClipBackfillDecision } from '../lib/task_api';

const offer = (overrides = {}) => ({
  migration_settled: true,
  decision: null,
  should_ask: true,
  never_indexed: 1200,
  stalled: 0,
  skipped_deleted: 340,
  failed_imports: 0,
  estimated_seconds: 3 * 3600 + 30 * 60,
  migration_status: 'completed_with_errors',
  diagnostics_deferred: false,
  ...overrides,
});

async function settle() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

describe('ClipBackfillDialog', () => {
  beforeEach(() => {
    getClipBackfillOffer.mockResolvedValue(offer());
    setClipBackfillDecision.mockImplementation(async (decision) =>
      offer({ decision, should_ask: false })
    );
  });

  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
    vi.clearAllMocks();
  });

  it('asks once the migration has settled and there is work', async () => {
    render(<ClipBackfillDialog />);
    await settle();

    expect(screen.getByText(/clipBackfill\.lead.*1200/)).toBeInTheDocument();
    expect(screen.getByText('clipBackfill.estimateHoursMinutes:{"hours":3,"minutes":30}'))
      .toBeInTheDocument();
  });

  // The distinction the old repair scan could not draw and the reason this
  // dialog exists: orphaned rows are what deleting screenshots looks like, and
  // reporting them as failures would alarm every user who has ever cleared
  // their history.
  it('reports deleted-screenshot skips apart from the images to encode', async () => {
    render(<ClipBackfillDialog />);
    await settle();

    expect(screen.getByText(/clipBackfill\.skippedDeleted.*340/)).toBeInTheDocument();
    expect(screen.queryByText(/clipBackfill\.failedImports/)).toBeNull();
    // The estimate covers what would be encoded, not what was skipped.
    expect(screen.getByText('1200')).toBeInTheDocument();
  });

  it('shows a genuine import failure when there was one', async () => {
    getClipBackfillOffer.mockResolvedValue(offer({ failed_imports: 4 }));
    render(<ClipBackfillDialog />);
    await settle();

    expect(screen.getByText(/clipBackfill\.failedImports.*4/)).toBeInTheDocument();
  });

  it('stays out of the way until the migration settles', async () => {
    getClipBackfillOffer.mockResolvedValue(
      offer({ migration_settled: false, should_ask: false })
    );
    render(<ClipBackfillDialog />);
    await settle();

    expect(screen.queryByText(/clipBackfill\.lead/)).toBeNull();
  });

  it('keeps polling while the migration is unsettled', async () => {
    vi.useFakeTimers();
    getClipBackfillOffer
      .mockResolvedValueOnce(offer({ migration_settled: false, should_ask: false }))
      .mockResolvedValueOnce(offer());
    render(<ClipBackfillDialog />);
    await settle();

    expect(getClipBackfillOffer).toHaveBeenCalledTimes(1);
    expect(screen.queryByText(/clipBackfill\.lead/)).toBeNull();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(15000);
    });

    expect(getClipBackfillOffer).toHaveBeenCalledTimes(2);
    expect(screen.getByText(/clipBackfill\.lead.*1200/)).toBeInTheDocument();
  });

  it('keeps polling while the expensive census waits for idle time', async () => {
    vi.useFakeTimers();
    getClipBackfillOffer
      .mockResolvedValueOnce(offer({
        should_ask: false,
        never_indexed: 0,
        diagnostics_deferred: true,
      }))
      .mockResolvedValueOnce(offer());
    render(<ClipBackfillDialog />);
    await settle();

    await act(async () => {
      vi.advanceTimersByTime(15000);
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(getClipBackfillOffer).toHaveBeenCalledTimes(2);
    expect(screen.getByText(/clipBackfill\.lead.*1200/)).toBeInTheDocument();
  });

  it('stops polling after a settled migration reports no work', async () => {
    vi.useFakeTimers();
    getClipBackfillOffer.mockResolvedValue(
      offer({ should_ask: false, never_indexed: 0, stalled: 0 })
    );
    render(<ClipBackfillDialog />);
    await settle();

    expect(getClipBackfillOffer).toHaveBeenCalledTimes(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(60000);
    });

    expect(getClipBackfillOffer).toHaveBeenCalledTimes(1);
  });

  it('retries after a transient offer read failure', async () => {
    vi.useFakeTimers();
    getClipBackfillOffer.mockRejectedValueOnce(new Error('locked'));
    render(<ClipBackfillDialog />);
    await settle();

    expect(getClipBackfillOffer).toHaveBeenCalledTimes(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(15000);
    });

    expect(getClipBackfillOffer).toHaveBeenCalledTimes(2);
    expect(screen.getByText(/clipBackfill\.lead.*1200/)).toBeInTheDocument();
  });

  it('does not ask again once an answer is recorded', async () => {
    getClipBackfillOffer.mockResolvedValue(
      offer({ decision: 'declined', should_ask: false })
    );
    render(<ClipBackfillDialog />);
    await settle();

    expect(screen.queryByText(/clipBackfill\.lead/)).toBeNull();
  });

  it('records either answer', async () => {
    render(<ClipBackfillDialog />);
    await settle();

    await act(async () => {
      fireEvent.click(screen.getByText('clipBackfill.approve'));
    });
    expect(setClipBackfillDecision).toHaveBeenCalledWith('approved');
    expect(screen.queryByText(/clipBackfill\.lead/)).toBeNull();
  });

  it('records a refusal without starting anything', async () => {
    render(<ClipBackfillDialog />);
    await settle();

    await act(async () => {
      fireEvent.click(screen.getByText('clipBackfill.decline'));
    });
    expect(setClipBackfillDecision).toHaveBeenCalledWith('declined');
  });
});
