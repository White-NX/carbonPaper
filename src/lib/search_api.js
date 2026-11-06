import { invoke } from '@tauri-apps/api/core';
import { withAuth } from './auth_api';
import { throwIfSearchAborted } from './latest_task_queue';

/** Unified text search. Timestamps at this boundary remain Unix seconds. */
export const searchSnapshotsAndRecaps = (query, options = {}) => withAuth(async () => {
  throwIfSearchAborted(options.signal);
  const response = await invoke('storage_search_unified', {
    request: {
      query,
      process_names: options.processNames || [],
      categories: options.categories || [],
      start_time: options.startTime ?? null,
      end_time: options.endTime ?? null,
      offset: options.offset || 0,
      limit: options.limit || 40,
      include_semantic: options.includeSemantic ?? false,
      target: options.target ?? 'mixed',
      ranking_revision: options.rankingRevision ?? null,
    },
  });
  throwIfSearchAborted(options.signal);
  return response;
}, { autoPrompt: false });
