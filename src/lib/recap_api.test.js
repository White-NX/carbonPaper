import { describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from './auth_api';
import { correctRecap, deleteRecap, generateRecap, getRecapDay, getRecapRecords, getRecapProgress, groupRecapActivities, sourceResult } from './recap_api';

vi.mock('./auth_api', () => ({ withAuth: vi.fn((action) => action()) }));

describe('recap API and occurrence display', () => {
  it('protects deletion and addresses only the selected date', async () => {
    await deleteRecap('2026-09-01');
    expect(invoke).toHaveBeenLastCalledWith('recap_delete', { date: '2026-09-01' });
    expect(withAuth).toHaveBeenLastCalledWith(expect.any(Function), { autoPrompt: true });
  });
  it('uses authenticated light reads and passes pagination scope without prompting', async () => {
    await getRecapDay('2026-10-01', { includeRecords: false, includeAttempts: false });
    expect(invoke).toHaveBeenLastCalledWith('recap_get_day', { date: '2026-10-01', includeRecords: false, includeAttempts: false });
    await getRecapRecords('2026-10-01', 123, 'cursor');
    expect(invoke).toHaveBeenLastCalledWith('recap_get_records', { date: '2026-10-01', batchStartMs: 123, cursor: 'cursor' });
    expect(withAuth).toHaveBeenLastCalledWith(expect.any(Function), { autoPrompt: false });
    await getRecapProgress('2026-10-01', { includeAttempts: false });
    expect(invoke).toHaveBeenLastCalledWith('recap_get_progress', { date: '2026-10-01', includeAttempts: false });
  });
  it('polls without opening an authentication dialog and protects edits', async () => {
    await getRecapDay('2026-09-01');
    expect(withAuth).toHaveBeenLastCalledWith(expect.any(Function), { autoPrompt: false });
    await getRecapProgress('2026-09-01');
    expect(invoke).toHaveBeenLastCalledWith('recap_get_progress', { date: '2026-09-01' });
    expect(withAuth).toHaveBeenLastCalledWith(expect.any(Function), { autoPrompt: false });
    await generateRecap('2026-09-01');
    expect(invoke).toHaveBeenLastCalledWith('recap_generate', { date: '2026-09-01', force: false });
    await correctRecap('2026-09-01', { kind: 'undo' });
    expect(withAuth).toHaveBeenLastCalledWith(expect.any(Function), { autoPrompt: true });
  });
  it('preserves task interruptions and long gaps in chronological order', () => {
    const a = (id, task, start) => ({ id, task_id: task, task_title: task, start_ms: start, end_ms: start + 1000, text: id });
    const groups = groupRecapActivities([{ activities: [a('4', 'A', 900000), a('1', 'A', 0), a('3', 'A', 20000), a('2', 'B', 10000)] }]);
    expect(groups.map((g) => g.task_id)).toEqual(['A', 'B', 'A', 'A']);
  });
  it('can join consecutive same-task occurrences across processing batches', () => {
    const a = { id: '1', task_id: 'a', task_title: 'Task', start_ms: 100, end_ms: 200, text: 'one' };
    const b = { ...a, id: '2', start_ms: 300, end_ms: 400 };
    expect(groupRecapActivities([{ activities: [a] }, { activities: [b] }])[0].activities).toHaveLength(2);
  });
  it('passes seconds to the existing screenshot selection path', () => {
    expect(sourceResult({ id: 42, timestamp_ms: 1790467200000, process_name: 'Editor' })).toMatchObject({ screenshot_id: 42, timestamp: 1790467200 });
  });
});
