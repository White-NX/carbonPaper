import React from 'react';
import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key) => key,
  }),
}));

vi.mock('../lib/task_api', () => ({
  getMinilmRebuildStatus: vi.fn(),
}));

vi.mock('../lib/auth_api', () => ({
  requestAuth: vi.fn(),
}));

import MinilmMigrationOverlay from './MinilmMigrationOverlay';
import { getMinilmRebuildStatus } from '../lib/task_api';
import { requestAuth } from '../lib/auth_api';

const runningStatus = (overrides = {}) => ({
  running: true,
  run_id: 'run-a',
  phase: 'copying_chroma',
  mode: 'copy_chroma_hot_layer',
  chroma_processed: 2,
  chroma_total: 10,
  failed: 0,
  unmappable: 0,
  discarded: 0,
  last_error: null,
  ...overrides,
});

// Advance the poll loop (setTimeout chain) and flush the async state updates.
async function pollTick(ms = 0) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

describe('MinilmMigrationOverlay', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.clearAllMocks();
  });

  it('shows a non-dismissable overlay for an active run', async () => {
    getMinilmRebuildStatus.mockResolvedValue(runningStatus());
    render(<MinilmMigrationOverlay />);
    await pollTick();

    expect(screen.getByText('minilmMigration.title')).toBeInTheDocument();
    expect(screen.getByText(/2\s*\/\s*10\s*\(20%\)/)).toBeInTheDocument();
    expect(screen.queryByRole('button')).toBeNull();
  });

  it('stays hidden when no migration is running', async () => {
    getMinilmRebuildStatus.mockResolvedValue(
      runningStatus({ running: false, phase: 'idle' })
    );
    render(<MinilmMigrationOverlay />);
    await pollTick();

    expect(screen.queryByText('minilmMigration.title')).toBeNull();
  });

  it('offers re-authentication while the run waits for Windows Hello', async () => {
    getMinilmRebuildStatus.mockResolvedValue(
      runningStatus({ phase: 'waiting_for_auth' })
    );
    render(<MinilmMigrationOverlay />);
    await pollTick();

    expect(screen.getByText('minilmMigration.waitingForAuth')).toBeInTheDocument();
    expect(
      screen.getByRole('button', { name: 'minilmMigration.reauth' })
    ).toBeEnabled();
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'minilmMigration.reauth' }));
    });
    expect(requestAuth).toHaveBeenCalledOnce();
  });
});
