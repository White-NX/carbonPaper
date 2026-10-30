import React from 'react';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import App from './App';
import zh from './i18n/locales/zh-CN.json';
import { searchSnapshotsAndRecaps } from './lib/search_api';
import { getScreenshotDetails, searchScreenshots } from './lib/monitor_api';

const mocks = vi.hoisted(() => ({ authenticated: true, copy: vi.fn(async () => {}), window: vi.fn() }));
vi.mock('react-i18next', () => ({ useTranslation: () => ({
  t: (key, options = {}) => {
    const value = key.split('.').reduce((object, part) => object?.[part], zh);
    if (options.returnObjects) return value;
    return typeof value === 'string' ? value.replace(/\{\{(\w+)\}\}/g, (_, name) => options[name] ?? '') : key;
  }, i18n: { language: 'zh-CN' },
}) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => vi.fn()) }));
vi.mock('./hooks/useAuthSession', () => ({ useAuthSession: () => ({ isAuthenticated: mocks.authenticated }) }));
vi.mock('./hooks/useAppTheme', () => ({ useAppTheme: () => ({ darkMode: true }) }));
vi.mock('./hooks/useAppWindowState', () => ({
  usePowerSavingState: () => ({ windowFocused: true }), useWindowMaximizedState: () => false, useAppWindowActions: () => ({}),
}));
vi.mock('./hooks/useAppNotifications', () => ({ useAppNotifications: () => ({ notifications: [], toastNotifications: [] }) }));
vi.mock('./hooks/useCriticalErrors', () => ({ useCriticalErrors: () => ({ criticalErrors: [] }) }));
vi.mock('./hooks/useRequiredModels', () => ({ useRequiredModels: () => ({ modelsCheckDone: true, modelsNeedDownload: false }) }));
vi.mock('./hooks/useRequiredModelDownload', () => ({ useRequiredModelDownload: () => ({}) }));
vi.mock('./hooks/useMonitorLifecycle', () => ({ useMonitorLifecycle: () => ({ backendStatus: 'online' }) }));
vi.mock('./hooks/useSettingsHost', () => ({ useSettingsHost: () => ({}) }));
vi.mock('./hooks/useUpdateManager', () => ({ useUpdateManager: () => ({}) }));
vi.mock('./hooks/useSnapshotPreviewWindow', () => ({ useSnapshotPreviewWindow: () => ({ tabs: [], open: mocks.window }) }));
vi.mock('./components/settings/hooks/useSavedCaptureFilters', () => ({ useSavedCaptureFilters: () => {} }));
vi.mock('./components/onboarding/useOnboarding', () => ({ useOnboarding: () => ({}) }));
vi.mock('./components/Mask', () => ({ default: () => null }));
vi.mock('./components/AuthMask', () => ({ default: () => null }));
vi.mock('./components/ErrorWindow', () => ({ default: () => null }));
vi.mock('./components/StartupVacuumDialog', () => ({ default: () => null }));
vi.mock('./components/VectorMigrationOverlay', () => ({ default: () => null }));
vi.mock('./components/HmacMigrationDialog', () => ({ default: () => null }));
vi.mock('./components/ClipBackfillDialog', () => ({ default: () => null }));
vi.mock('./components/onboarding/OnboardingOverlay', () => ({ default: () => null }));
vi.mock('./components/UpdateModal', () => ({ UpdateModal: () => null }));
vi.mock('./components/AppBoundUpgradePrompt', () => ({ default: () => null }));
vi.mock('./components/Notifications', () => ({ NotificationToast: () => null, NotificationPanel: () => null }));
vi.mock('./components/OcrModelRepairCard', () => ({ default: () => null }));
vi.mock('./components/WorkspaceTimelines', () => ({ default: () => null }));
vi.mock('./components/SmartClustersView', () => ({ default: () => null }));
vi.mock('./components/recap/RecapView', () => ({ default: () => null }));
vi.mock('./components/InspectorImage', () => ({ InspectorImage: ({ item }) => <img src={item.imageUrl} alt="Screenshot" /> }));
vi.mock('./components/OfficeDocumentEntry', () => ({ default: () => null }));
vi.mock('./lib/recap_api', async (original) => ({ ...await original(), getSnapshotRecapContext: vi.fn(async () => ({ status: 'unmatched', items: [] })) }));
vi.mock('./lib/monitor_api', () => ({
  warmupThumbnails: vi.fn(async () => {}), fetchImage: vi.fn(async () => 'screenshot.png'), fetchThumbnail: vi.fn(async () => null),
  fetchThumbnailBatch: vi.fn(async () => ({})), getScreenshotDetails: vi.fn(async () => ({
    record: { id: 42, process_name: 'Editor', window_title: 'Design notes', created_at: '2026-10-28T02:00:00Z' }, ocr_results: [{ text: 'Matched design' }],
  })), listProcesses: vi.fn(async () => []), getCategoriesFromDb: vi.fn(async () => []), listRecentScreenshots: vi.fn(async () => []),
  searchScreenshots: vi.fn(async () => []), batchGetCategories: vi.fn(async () => ({})), computeLinkScores: vi.fn(async () => []),
  updateScreenshotCategory: vi.fn(), resumeOfficeDocument: vi.fn(),
  getSoftDeleteQueueStatus: vi.fn(async () => ({ pending_screenshots: 0, pending_ocr: 0, running: false })),
  getBackgroundIndexProgress: vi.fn(async () => ({})),
}));
vi.mock('./lib/search_api', () => ({ searchSnapshotsAndRecaps: vi.fn() }));

beforeEach(() => {
  localStorage.clear(); mocks.authenticated = true;
  invoke.mockImplementation(async (command) => command === 'storage_check_hmac_migration_status'
    ? { needs_migration: false, is_running: false }
    : null);
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: mocks.copy } });
  searchSnapshotsAndRecaps.mockResolvedValue({
    items: [{ kind: 'snapshot', snapshot: { screenshot_id: 42, process_name: 'Editor', window_title: 'Design notes', text: 'Matched design', captured_at_ms: Date.parse('2026-10-28T02:00:00Z') } }],
    total: 1, snapshot_count: 1, recap_count: 0, next_offset: null, semantic_status: 'disabled',
  });
});

it('returns from timeline navigation to the same search result with source focus restored', async () => {
  render(<App />);
  const quickSearch = screen.getByRole('textbox', { name: '搜索截图里出现过的文字' });
  fireEvent.change(quickSearch, { target: { value: 'design' } });
  fireEvent.keyDown(quickSearch, { key: 'Enter' });
  const field = await screen.findByRole('textbox', { name: '搜索历史记录' });
  expect(field).toHaveFocus();
  await waitFor(() => expect(field).toHaveValue('design'));
  const source = await screen.findByRole('button', { name: /Design notes/ });
  source.focus(); fireEvent.click(source);
  const preview = await screen.findByRole('dialog', { name: '快照预览' });
  const copy = within(preview).getByRole('button', { name: '复制文字' });
  copy.focus();
  fireEvent.keyDown(window, { key: 'k', ctrlKey: true });
  expect(copy).toHaveFocus();
  fireEvent.click(within(preview).getByRole('button', { name: '复制文字' }));
  await waitFor(() => expect(mocks.copy).toHaveBeenCalledWith('Matched design'));
  fireEvent.click(within(preview).getByRole('button', { name: '在时间线上查看' }));
  expect(screen.getByRole('heading', { name: '记录' })).toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: '返回搜索' }));
  await waitFor(() => expect(source).toHaveFocus());
  expect(field).toHaveValue('design');
  expect(searchSnapshotsAndRecaps).toHaveBeenCalledOnce();
});

it('opens a title-bar quick-search result in the records viewer', async () => {
  searchScreenshots.mockResolvedValueOnce([{
    screenshot_id: 42, process_name: 'Quick result', window_title: 'Quick design notes',
    text: 'Matched design', created_at: '2026-10-28T02:00:00Z',
  }]);
  render(<App />);
  const input = screen.getByRole('textbox', { name: '搜索截图里出现过的文字' });
  act(() => input.focus());
  fireEvent.change(input, { target: { value: 'design' } });
  fireEvent.click(await screen.findByText('Quick design notes'));
  await waitFor(() => expect(getScreenshotDetails).toHaveBeenCalledWith(42, undefined));
  expect(screen.getByRole('heading', { name: '记录' })).toBeInTheDocument();
  expect(await screen.findByRole('img', { name: 'Screenshot' })).toBeInTheDocument();
  expect(screen.queryByText('Quick design notes')).not.toBeInTheDocument();
});

it('submits the picture-search query and mode from the title bar to full search', async () => {
  render(<App />);
  fireEvent.click(screen.getByRole('button', { name: '切换到画面搜索' }));
  const input = screen.getByRole('textbox', { name: '描述你想找的画面' });
  fireEvent.change(input, { target: { value: 'moon' } });
  fireEvent.keyDown(input, { key: 'Enter' });
  const field = await screen.findByRole('textbox', { name: '搜索历史记录' });
  await waitFor(() => expect(field).toHaveValue('moon'));
  await waitFor(() => expect(searchScreenshots).toHaveBeenCalledWith('moon', 'nl', expect.any(Object)));
  expect(searchSnapshotsAndRecaps).not.toHaveBeenCalled();
});

it('focuses title-bar quick search without leaving the current workspace', () => {
  render(<App />);
  fireEvent.keyDown(window, { key: 'k', ctrlKey: true });
  expect(screen.getByRole('textbox', { name: '搜索截图里出现过的文字' })).toHaveFocus();
  expect(screen.getByRole('heading', { name: '记录' })).toBeInTheDocument();
});

it('reapplies the submitted quick-search query after editing the full-search field', async () => {
  const clock = vi.spyOn(Date, 'now').mockReturnValue(1234567890);
  try {
    render(<App />);
    const input = screen.getByRole('textbox', { name: '搜索截图里出现过的文字' });
    fireEvent.change(input, { target: { value: 'design' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    const field = await screen.findByRole('textbox', { name: '搜索历史记录' });
    await waitFor(() => expect(field).toHaveValue('design'));
    fireEvent.change(field, { target: { value: 'edited' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    await waitFor(() => expect(field).toHaveValue('design'));
  } finally {
    clock.mockRestore();
  }
});

it('keeps the global search shortcut disabled while locked', () => {
  mocks.authenticated = false;
  render(<App />);
  fireEvent.keyDown(window, { key: 'k', ctrlKey: true });
  expect(screen.getByRole('heading', { name: '记录' })).toBeInTheDocument();
  const input = screen.getByRole('textbox', { name: '搜索截图里出现过的文字' });
  expect(input).toBeDisabled();
  expect(input).not.toHaveFocus();
});
