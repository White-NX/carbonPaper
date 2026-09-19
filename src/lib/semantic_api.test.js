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
  getMinilmRebuildStatus,
  listMinilmRebuildErrors,
  getClipRebuildStatus,
  getBlindIndexRepairStatus,
  listClipRebuildErrors,
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

  it('exposes read-only MiniLM migration status commands', async () => {
    invoke.mockResolvedValue({ running: true });

    await getMinilmRebuildStatus();
    await getBlindIndexRepairStatus();
    await listMinilmRebuildErrors(5, 25);
    await getMaintenanceStatus();

    expect(invoke).toHaveBeenNthCalledWith(1, 'get_minilm_rebuild_status');
    expect(invoke).toHaveBeenNthCalledWith(2, 'get_blind_index_repair_status');
    expect(invoke).toHaveBeenNthCalledWith(3, 'list_minilm_rebuild_errors', {
      offset: 5,
      limit: 25,
    });
    expect(invoke).toHaveBeenNthCalledWith(4, 'get_maintenance_status');
    expectWithAuth(1);
  });

  it('exposes the same pair for the CLIP migration', async () => {
    invoke.mockResolvedValue({ running: true });

    await getClipRebuildStatus();
    await listClipRebuildErrors(0, 100);

    // Status is unauthenticated so the overlay can poll it before unlock —
    // `waiting_for_auth` is one of the phases it has to be able to render.
    expect(invoke).toHaveBeenNthCalledWith(1, 'get_clip_rebuild_status');
    expect(invoke).toHaveBeenNthCalledWith(2, 'list_clip_rebuild_errors', {
      offset: 0,
      limit: 100,
    });
    expectWithAuth(1);
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
