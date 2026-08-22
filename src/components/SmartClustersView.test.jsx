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
  renameSmartCluster: vi.fn(),
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

import SmartClustersView, { parseMarkdownBlocks } from './SmartClustersView';
import {
  deleteSmartCluster,
  getSmartClusterAssignments,
  getSmartClusterStatus,
  listSmartClusters,
  smartClusterDrainNow,
} from '../lib/task_api';
import { getSmartClusterWorkerStatus } from '../lib/monitor_api';

const cluster = {
  id: 7,
  anchor_text: 'Research notes',
  display_name: 'Research notes',
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
  // 行内操作收进了「更多」菜单，删除要先把菜单打开。
  await user.click(screen.getByTitle('smartClusters.rowMenu'));
  await user.click(screen.getByRole('button', { name: 'clusterCard.actionDelete' }));
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

describe('SmartClustersView manual drain', () => {
  beforeEach(() => {
    listSmartClusters.mockResolvedValue([cluster]);
    getSmartClusterStatus.mockResolvedValue({
      pending_count: 2,
      enabled_cluster_count: 1,
      total_cluster_count: 1,
    });
    getSmartClusterWorkerStatus.mockResolvedValue({
      running: false,
      forceRunning: false,
      pending_count: 2,
      unverifiableThresholds: 0,
    });
    smartClusterDrainNow.mockResolvedValue({ status: 'success' });
  });

  it('shows the requested drain immediately and exposes stop while it is admitted', async () => {
    const user = userEvent.setup();
    renderView();

    await user.click(await screen.findByRole('button', { name: 'smartClusters.processNow' }));

    expect(smartClusterDrainNow).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('button', { name: 'smartClusters.stopDrain' })).toBeInTheDocument();
  });

  it('renders a backend error instead of silently ignoring a failed request', async () => {
    smartClusterDrainNow.mockRejectedValue(new Error('Background scheduler is unavailable'));
    const user = userEvent.setup();
    renderView();

    await user.click(await screen.findByRole('button', { name: 'smartClusters.processNow' }));

    expect(await screen.findByText('Background scheduler is unavailable')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'smartClusters.processNow' })).toBeInTheDocument();
  });
});

describe('SmartClustersView assignments pagination', () => {
  beforeEach(() => {
    listSmartClusters.mockResolvedValue([cluster]);
    getSmartClusterAssignments.mockResolvedValue([]);
  });

  it('loads the first assignment page when a cluster is selected', async () => {
    const user = userEvent.setup();
    renderView();

    await user.click(await screen.findByRole('button', { name: 'Research notes' }));

    await waitFor(() => {
      expect(getSmartClusterAssignments).toHaveBeenCalledWith(7, 0, 50);
    });
  });
});

describe('SmartClustersView cluster metadata', () => {
  it('renders the backend last process and window title as the subtitle', async () => {
    listSmartClusters.mockResolvedValue([{
      ...cluster,
      last_process_name: 'chrome.exe',
      last_window_title: 'Research notes',
    }]);

    renderView();

    expect(await screen.findByText('chrome.exe · Research notes')).toBeInTheDocument();
  });
});

describe('smart cluster markdown rendering', () => {
  it('parses headings, lists, and fenced code as separate blocks', () => {
    expect(parseMarkdownBlocks([
      '# Summary',
      '',
      '- first point',
      '- second point',
      '',
      '1. first step',
      '2. second step',
      '',
      '```text',
      'const answer = 42;',
      '```',
    ].join('\n'))).toEqual([
      { type: 'heading', level: 1, text: 'Summary' },
      { type: 'ul', items: ['first point', 'second point'] },
      { type: 'ol', items: ['first step', 'second step'] },
      { type: 'code', text: 'const answer = 42;' },
    ]);
  });

  it('renders markdown blocks with list and code semantics', async () => {
    listSmartClusters.mockResolvedValue([{
      ...cluster,
      summary: {
        title: 'Summary',
        summary: '# Findings\n\n- First finding\n- Second finding\n\n```text\nconst answer = 42;\n```',
      },
    }]);
    getSmartClusterAssignments.mockResolvedValue([]);

    const user = userEvent.setup();
    renderView();
    await user.click(await screen.findByRole('button', { name: 'Research notes' }));

    expect(await screen.findByText('Findings')).toHaveClass('font-bold');
    expect(screen.getByRole('list', { name: '' })).toBeInTheDocument();
    expect(screen.getByText('First finding')).toBeInTheDocument();
    expect(screen.getByText('const answer = 42;')).toBeInTheDocument();
    expect(screen.getByText('const answer = 42;').closest('code')).toBeInTheDocument();
  });

  it('keeps safe links and evidence citations interactive inside summaries', async () => {
    listSmartClusters.mockResolvedValue([{
      ...cluster,
      summary: {
        title: 'Summary',
        summary: 'Read [the source](https://example.com) [1].',
        evidence: [{ screenshot_id: 42, label: 'Source snapshot' }],
      },
    }]);
    getSmartClusterAssignments.mockResolvedValue([]);

    const user = userEvent.setup();
    renderView();
    await user.click(await screen.findByRole('button', { name: 'Research notes' }));

    const link = await screen.findByRole('link', { name: 'the source' });
    expect(link).toHaveAttribute('href', 'https://example.com');
    expect(link).toHaveAttribute('target', '_blank');
    expect(screen.getByRole('button', { name: '1' })).toHaveAttribute('title', '#42');
  });
});
