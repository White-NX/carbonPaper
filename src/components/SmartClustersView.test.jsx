import React from 'react';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({ t: (key) => key }),
}));

vi.mock('../lib/task_api', () => ({
  createSmartCluster: vi.fn(),
  deleteSmartCluster: vi.fn(),
  getSmartClusterAssignments: vi.fn(async () => []),
  getSmartClusterStatus: vi.fn(async () => ({})),
  listSmartClusters: vi.fn(),
  smartClusterDrainNow: vi.fn(),
  smartClusterStopDrain: vi.fn(),
  toggleSmartClusterEnabled: vi.fn(),
  updateSmartClusterAnchor: vi.fn(),
}));

vi.mock('../lib/monitor_api', () => ({
  fetchThumbnailBatch: vi.fn(async () => ({})),
  getSmartClusterWorkerStatus: vi.fn(async () => ({
    running: false,
    forceRunning: false,
    pending_count: 0,
    unverifiableThresholds: 0,
  })),
}));

import SmartClustersView from './SmartClustersView';
import { deleteSmartCluster, listSmartClusters } from '../lib/task_api';

const cluster = {
  id: 7,
  anchor_text: 'Research notes',
  assignment_count: 0,
  threshold: 0.8,
  enabled: true,
};

function renderView() {
  return render(
    <SmartClustersView
      isAuthenticated
      active
      onSelectScreenshot={vi.fn()}
    />
  );
}

async function openDeleteDialog(user) {
  await screen.findByText('Research notes');
  await user.click(screen.getByTitle('clusterCard.actionDelete'));
  return screen.findByRole('dialog');
}

describe('SmartClustersView deletion confirmation', () => {
  beforeEach(() => {
    listSmartClusters.mockResolvedValue([cluster]);
    deleteSmartCluster.mockResolvedValue(undefined);
  });

  it('cancels without deleting the cluster', async () => {
    const user = userEvent.setup();
    renderView();
    await openDeleteDialog(user);

    await user.click(screen.getByRole('button', { name: 'common.cancel' }));

    expect(deleteSmartCluster).not.toHaveBeenCalled();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('keeps the confirmation locked while deletion is in progress', async () => {
    let resolveDelete;
    deleteSmartCluster.mockImplementation(() => new Promise((resolve) => {
      resolveDelete = resolve;
    }));
    const user = userEvent.setup();
    renderView();
    await openDeleteDialog(user);

    await user.click(screen.getByRole('button', { name: 'common.confirm' }));

    expect(screen.getByRole('button', { name: 'common.processing' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'common.cancel' })).toBeDisabled();

    resolveDelete();
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(deleteSmartCluster).toHaveBeenCalledWith(7);
  });

  it('dismisses the confirmation and requests unlock when authentication expires', async () => {
    deleteSmartCluster.mockRejectedValue(new Error('AUTH_REQUIRED'));
    const authRequired = vi.fn();
    window.addEventListener('cp-auth-required', authRequired);
    const user = userEvent.setup();
    renderView();
    await openDeleteDialog(user);

    await user.click(screen.getByRole('button', { name: 'common.confirm' }));

    await waitFor(() => expect(authRequired).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    window.removeEventListener('cp-auth-required', authRequired);
  });
});
