import React from 'react';
import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key) => key,
  }),
}));

vi.mock('../lib/task_api', () => ({
  getMaintenanceStatus: vi.fn(),
  getMinilmRebuildStatus: vi.fn(),
  getClipRebuildStatus: vi.fn(),
}));

vi.mock('../lib/auth_api', () => ({
  requestAuth: vi.fn(),
}));

import VectorMigrationOverlay from './VectorMigrationOverlay';
import {
  getClipRebuildStatus,
  getMaintenanceStatus,
  getMinilmRebuildStatus,
} from '../lib/task_api';
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

describe('VectorMigrationOverlay', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    getMaintenanceStatus.mockResolvedValue({ active: true, reason: 'minilm_migration' });
    getMinilmRebuildStatus.mockResolvedValue(runningStatus());
    getClipRebuildStatus.mockResolvedValue(runningStatus());
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.clearAllMocks();
  });

  it('shows a non-dismissable overlay for an active MiniLM run', async () => {
    render(<VectorMigrationOverlay />);
    await pollTick();

    expect(screen.getByText('vectorMigration.kinds.minilm.title')).toBeInTheDocument();
    expect(screen.getByText(/2\s*\/\s*10\s*\(20%\)/)).toBeInTheDocument();
    expect(screen.queryByRole('button')).toBeNull();
  });

  // The regression this component was generalized for: the CLIP migration held
  // the maintenance guard and rejected the monitor commands with nothing on
  // screen, because the overlay only ever polled MiniLM.
  it('shows the image-index box while the CLIP migration holds maintenance', async () => {
    getMaintenanceStatus.mockResolvedValue({ active: true, reason: 'clip_migration' });
    render(<VectorMigrationOverlay />);
    await pollTick();

    expect(screen.getByText('vectorMigration.kinds.clip.title')).toBeInTheDocument();
    expect(getClipRebuildStatus).toHaveBeenCalled();
    expect(getMinilmRebuildStatus).not.toHaveBeenCalled();
  });

  it('shows the image-index box for an ANN-only bootstrap without a migration run', async () => {
    getMaintenanceStatus.mockResolvedValue({ active: true, reason: 'clip_ann_bootstrap' });
    render(<VectorMigrationOverlay />);
    await pollTick();

    expect(screen.getByText('vectorMigration.kinds.clip.title')).toBeInTheDocument();
    expect(screen.getByText('vectorMigration.phases.building_ann')).toBeInTheDocument();
    expect(getClipRebuildStatus).not.toHaveBeenCalled();
    expect(getMinilmRebuildStatus).not.toHaveBeenCalled();
  });

  it('stays hidden when the app is not in maintenance mode', async () => {
    getMaintenanceStatus.mockResolvedValue({ active: false, reason: null });
    render(<VectorMigrationOverlay />);
    await pollTick();

    expect(screen.queryByText('vectorMigration.kinds.minilm.title')).toBeNull();
    expect(screen.queryByText('vectorMigration.kinds.unknown.title')).toBeNull();
    // No detail read is worth making when the cheap outer flag already says no.
    expect(getMinilmRebuildStatus).not.toHaveBeenCalled();
    expect(getClipRebuildStatus).not.toHaveBeenCalled();
  });

  it('still explains itself when the reason is unrecognised', async () => {
    getMaintenanceStatus.mockResolvedValue({ active: true, reason: 'something_new' });
    render(<VectorMigrationOverlay />);
    await pollTick();

    expect(screen.getByText('vectorMigration.kinds.unknown.title')).toBeInTheDocument();
  });

  it('keeps the box when the detailed status read fails', async () => {
    getMinilmRebuildStatus.mockRejectedValue(new Error('database locked'));
    render(<VectorMigrationOverlay />);
    await pollTick();

    expect(screen.getByText('vectorMigration.kinds.minilm.title')).toBeInTheDocument();
    expect(screen.getByText('vectorMigration.phases.starting')).toBeInTheDocument();
  });

  it('offers re-authentication while the run waits for Windows Hello', async () => {
    getMinilmRebuildStatus.mockResolvedValue(runningStatus({ phase: 'waiting_for_auth' }));
    render(<VectorMigrationOverlay />);
    await pollTick();

    expect(screen.getByText('vectorMigration.waitingForAuth')).toBeInTheDocument();
    expect(
      screen.getByRole('button', { name: 'vectorMigration.reauth' })
    ).toBeEnabled();
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'vectorMigration.reauth' }));
    });
    expect(requestAuth).toHaveBeenCalledOnce();
  });
});
