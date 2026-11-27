import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from './auth_api';
import { getUsageAppDetails, getUsageDay, setUsageAppCategory, moveUsageDate, usageHourlySelection } from './usage_api';

vi.mock('./auth_api', () => ({ withAuth: vi.fn((operation) => operation()) }));
beforeEach(() => { invoke.mockReset(); invoke.mockResolvedValue({}); });

describe('usage API', () => {
  it('keeps reads authenticated and preserves the selected civil date and application', async () => {
    await getUsageDay('2026-11-24');
    await getUsageAppDetails('2026-11-24', 'endfield.exe');
    expect(withAuth).toHaveBeenCalledWith(expect.any(Function), { autoPrompt: false });
    expect(invoke).toHaveBeenNthCalledWith(1, 'usage_get_day', { date: '2026-11-24' });
    expect(invoke).toHaveBeenNthCalledWith(2, 'usage_get_app_details', { date: '2026-11-24', appId: 'endfield.exe' });
  });

  it('uses UTC bounds for selected-app bars, including repeated DST hours and midnight clipping', () => {
    const start = Date.UTC(2026, 10, 1, 4);
    const hours = Array.from({ length: 25 }, (_, index) => ({ start_ms: start + index * 3600000, end_ms: start + (index + 1) * 3600000, total_ms: 3600000 }));
    const day = { hours, intervals: [
      { state: 'active', app_id: 'game.exe', start_ms: start - 1000, end_ms: start + 2.5 * 3600000 },
      { state: 'active', app_id: 'browser.exe', start_ms: start + 2.5 * 3600000, end_ms: start + 25 * 3600000 },
      { state: 'idle', app_id: 'game.exe', start_ms: start + 5 * 3600000, end_ms: start + 6 * 3600000 },
    ] };
    expect(usageHourlySelection(day, 'game.exe')).toEqual([3600000, 3600000, 1800000, ...Array(22).fill(0)]);
    expect(usageHourlySelection(day, null)).toHaveLength(25);
    expect(moveUsageDate('2026-03-08', -1)).toBe('2026-03-07');
    expect(moveUsageDate('2026-12-31', 1)).toBe('2027-01-01');
  });

  it('authenticates category changes and preserves the archive generation when restoring automatic classification', async () => {
    await setUsageAppCategory('example.exe', '游戏', 7);
    await setUsageAppCategory('example.exe', null, 7);
    expect(withAuth).toHaveBeenCalledWith(expect.any(Function), { autoPrompt: false });
    expect(invoke).toHaveBeenNthCalledWith(1, 'usage_set_app_category', { appId: 'example.exe', category: '游戏', generation: 7 });
    expect(invoke).toHaveBeenNthCalledWith(2, 'usage_set_app_category', { appId: 'example.exe', category: null, generation: 7 });
  });
});
