import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';

vi.mock('./auth_api', () => ({
  withAuth: vi.fn(async (fn) => fn()),
  requestAuth: vi.fn(),
  checkAuthSession: vi.fn(),
  initAuthListeners: vi.fn(),
  lockSession: vi.fn(),
}));

import { withAuth } from './auth_api';
import {
  getSmartClusterOcrCorpus,
  getSmartClusterSummary,
  upsertSmartClusterSummary,
  deleteSmartClusterSummary,
  getBlindIndexRepairStatus,
  getMaintenanceStatus,
} from './semantic_api';

describe('semantic_api', () => {
  beforeEach(() => {
    invoke.mockReset();
    withAuth.mockClear();
  });

  const expectWithAuth = (callNumber, options) => {
    const call = withAuth.mock.calls[callNumber - 1];
    expect(call?.[0]).toEqual(expect.any(Function));
    if (options === undefined) {
      expect(call).toHaveLength(1);
    } else {
      expect(call?.[1]).toEqual(options);
    }
  };

  it('exposes read-only maintenance status commands', async () => {
    invoke.mockResolvedValue({ running: true });

    await getBlindIndexRepairStatus();
    await getMaintenanceStatus();

    // Both are unauthenticated so the overlay can poll them before unlock.
    expect(invoke).toHaveBeenNthCalledWith(1, 'get_blind_index_repair_status');
    expect(invoke).toHaveBeenNthCalledWith(2, 'get_maintenance_status');
    expect(withAuth).not.toHaveBeenCalled();
  });

  it('calls smart cluster summary commands with expected payloads', async () => {
    invoke.mockResolvedValue({});

    const summary = {
      smart_cluster_id: 7,
      title: 'MCP work',
      summary: 'Summarized smart cluster',
      ocr_summary: 'OCR mentions MCP tools',
    };

    await getSmartClusterOcrCorpus(7, 1, 25);
    await getSmartClusterSummary(7);
    await upsertSmartClusterSummary(summary);
    await deleteSmartClusterSummary(7);

    expect(invoke).toHaveBeenNthCalledWith(1, 'smart_cluster_ocr_corpus', {
      clusterId: 7,
      page: 1,
      pageSize: 25,
    });
    expect(invoke).toHaveBeenNthCalledWith(2, 'smart_cluster_get_summary', { clusterId: 7 });
    expect(invoke).toHaveBeenNthCalledWith(3, 'smart_cluster_upsert_summary', { summary });
    expect(invoke).toHaveBeenNthCalledWith(4, 'smart_cluster_delete_summary', { clusterId: 7 });
    expectWithAuth(1);
    expectWithAuth(2);
    expectWithAuth(3, { autoPrompt: true });
    expectWithAuth(4, { autoPrompt: true });
  });
});
