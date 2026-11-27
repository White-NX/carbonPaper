import { invoke } from '@tauri-apps/api/core';
import { withAuth } from './auth_api';

const read = (command, args) => withAuth(() => invoke(command, args), { autoPrompt: false });
export const getUsageDay = (date) => read('usage_get_day', { date });
export const getUsageRecordedDates = (firstDate, lastDate) => read('usage_get_recorded_dates', { firstDate, lastDate });
export const getUsageAppDetails = (date, appId) => read('usage_get_app_details', { date, appId });
export const setUsageAppCategory = (appId, category, generation) => withAuth(
  () => invoke('usage_set_app_category', { appId, category, generation }), { autoPrompt: false },
);

export function moveUsageDate(date, days) {
  const value = new Date(`${date}T12:00:00Z`);
  value.setUTCDate(value.getUTCDate() + days);
  return value.toISOString().slice(0, 10);
}

export function usageDuration(ms, t) {
  if (!ms) return t('usage.durationMinutes', { minutes: 0 });
  if (ms < 60000) return t('usage.lessThanMinute');
  const minutes = Math.floor(ms / 60000);
  const hours = Math.floor(minutes / 60);
  return hours ? t('usage.durationHoursMinutes', { hours, minutes: minutes % 60 }) : t('usage.durationMinutes', { minutes });
}

/** Use the saved UTC hour bounds, including repeated/skipped DST hours. */
export function usageHourlySelection(day, appId) {
  if (!appId) return (day?.hours || []).map((hour) => hour.total_ms);
  return (day?.hours || []).map((hour) => (day.intervals || []).reduce((total, interval) => {
    if (interval.state !== 'active' || interval.app_id !== appId) return total;
    return total + Math.max(0, Math.min(interval.end_ms, hour.end_ms) - Math.max(interval.start_ms, hour.start_ms));
  }, 0));
}

export function usageErrorKey(error) {
  const code = String(error?.message ?? error ?? '').match(/\b(AUTH_REQUIRED|MAINTENANCE_IN_PROGRESS|USAGE_SOURCE_CHANGED|USAGE_QUERY_FAILED|USAGE_INVALID_APP|USAGE_INVALID_CATEGORY)\b/)?.[1];
  return `usage.errors.${code || 'unknown'}`;
}
