import { describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from './auth_api';
import { searchSnapshotsAndRecaps } from './search_api';

vi.mock('./auth_api', () => ({ withAuth: vi.fn((action) => action()) }));

describe('unified search options', () => {
  it('defaults to mixed keyword retrieval without prompting for unlock', async () => {
    await searchSnapshotsAndRecaps('notes');
    expect(invoke).toHaveBeenLastCalledWith('storage_search_unified', { request: expect.objectContaining({
      query: 'notes', target: 'mixed', include_semantic: false,
    }) });
    expect(withAuth).toHaveBeenLastCalledWith(expect.any(Function), { autoPrompt: false });
  });

  it('passes recap scope, vector opt-in and Unix-second filters through pagination', async () => {
    await searchSnapshotsAndRecaps('decision', { target: 'recaps', includeSemantic: true,
      offset: 40, limit: 20, processNames: ['code.exe'], startTime: 100, endTime: 200, rankingRevision: 'a'.repeat(64) });
    expect(invoke).toHaveBeenLastCalledWith('storage_search_unified', { request: {
      query: 'decision', target: 'recaps', include_semantic: true, ranking_revision: 'a'.repeat(64), offset: 40, limit: 20,
      process_names: ['code.exe'], categories: [], start_time: 100, end_time: 200,
    } });
  });
});
