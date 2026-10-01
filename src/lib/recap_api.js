import { invoke } from '@tauri-apps/api/core';
import { withAuth } from './auth_api';

const read = (command, args) => withAuth(() => invoke(command, args), { autoPrompt: false });
const write = (command, args) => withAuth(() => invoke(command, args), { autoPrompt: true });

export const getRecapSettings = () => read('recap_get_settings');
export const saveRecapSettings = (settings) => write('recap_save_settings', { settings });
export const listRecapDays = () => read('recap_list_days');
export const getRecapDay = (date, options = {}) => read('recap_get_day', { date, ...options });
export const getRecapRecords = (date, batchStartMs = null, cursor = null) => read('recap_get_records', { date, batchStartMs, cursor });
export const getRecapProgress = (date, options = {}) => read('recap_get_progress', { date, ...options });
export const generateRecap = (date, force = false) => write('recap_generate', { date, force });
export const cancelRecap = () => invoke('recap_cancel');
export const correctRecap = (date, correction) => write('recap_correct', { date, correction });

export function localDate(date = new Date()) {
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`;
}

export function sourceResult(source) {
  return { screenshot_id: source.id, timestamp: source.timestamp_ms / 1000, process_name: source.process_name, window_title: source.window_title };
}

export function recapErrorKey(error) {
  const code = String(error?.message ?? error ?? '').match(/\b(RECAP_[A-Z_]+|AI_[A-Z_]+|AUTH_REQUIRED|MAINTENANCE_IN_PROGRESS)/)?.[1];
  return `recap.errors.${code || 'unknown'}`;
}

/** Join adjacent displayed occurrences only when the task and description agree.
 * Keep each original activity so correction controls still address backend IDs.
 */
export function groupRecapActivities(batches) {
  const activities = batches.flatMap((b) => b.activities || []).sort((a, b) => a.start_ms - b.start_ms);
  const groups = [];
  for (const activity of activities) {
    const last = groups.at(-1);
    if (last && last.task_id === activity.task_id && activity.start_ms - last.end_ms <= 600000) {
      last.end_ms = Math.max(last.end_ms, activity.end_ms);
      last.activities.push(activity);
    } else {
      groups.push({ task_id: activity.task_id, task_title: activity.task_title, start_ms: activity.start_ms, end_ms: activity.end_ms, activities: [activity] });
    }
  }
  return groups;
}
