import { invoke } from '@tauri-apps/api/core';
import { withAuth } from './auth_api';

/** Unified text search. Timestamps at this boundary remain Unix seconds. */
export const searchSnapshotsAndRecaps = (query, options = {}) => withAuth(() => invoke('storage_search_unified', {
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
}), { autoPrompt: false });
