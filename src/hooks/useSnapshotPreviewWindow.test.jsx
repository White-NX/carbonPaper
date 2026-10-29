import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, expect, it, vi } from 'vitest';
import { useSnapshotPreviewWindow } from './useSnapshotPreviewWindow';
import { SNAPSHOT_PREVIEW_WINDOW_STATE_KEY } from '../lib/snapshot_preview';

const mocks = vi.hoisted(() => ({ listeners: new Map(), windows: [], existing: null, focus: vi.fn(async () => {}), show: vi.fn(async () => {}) }));
vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async (event, callback) => { mocks.listeners.set(event, callback); return () => mocks.listeners.delete(event); }) }));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => ({ show: mocks.show, setFocus: mocks.focus }) }));
vi.mock('@tauri-apps/api/webviewWindow', () => ({ WebviewWindow: class {
  static async getByLabel() { return mocks.existing; }
  constructor(label, options) { this.label = label; this.options = options; this.callbacks = new Map(); this.emit = vi.fn(async () => {}); this.show = vi.fn(async () => {}); this.setFocus = vi.fn(async () => {}); mocks.windows.push(this); }
  async once(event, callback) { this.callbacks.set(event, callback); }
} }));
beforeEach(() => { localStorage.clear(); mocks.listeners.clear(); mocks.windows = []; mocks.existing = null; });

it('creates one native window and sends the newest bounded tabs after creation', async () => {
  const { result } = renderHook(() => useSnapshotPreviewWindow({ isAuthenticated: true }));
  act(() => {
    for (let id = 1; id <= 10; id++) result.current.open({ screenshot_id: id, thumbnailSrc: 'data:image/private' });
  });
  await waitFor(() => expect(mocks.windows).toHaveLength(1));
  expect(result.current.tabs).toHaveLength(8);
  expect(result.current.activeKey).toBe('id:10');
  expect(localStorage.getItem(SNAPSHOT_PREVIEW_WINDOW_STATE_KEY)).not.toContain('data:image');
  await act(async () => mocks.windows[0].callbacks.get('tauri://created')());
  expect(mocks.windows[0].emit).toHaveBeenCalledWith('snapshot-preview-state', expect.objectContaining({ activeKey: 'id:10' }));
  expect(mocks.windows[0].emit.mock.calls[0][1].tabs).toHaveLength(8);
  act(() => mocks.windows[0].callbacks.get('tauri://destroyed')());
  expect(result.current.tabs).toHaveLength(0);
});

it('updates an existing preview without creating another window and stops while locked', async () => {
  mocks.existing = { emit: vi.fn(async () => {}), show: vi.fn(async () => {}), setFocus: vi.fn(async () => {}) };
  const { result, rerender } = renderHook(({ authenticated }) => useSnapshotPreviewWindow({ isAuthenticated: authenticated }), { initialProps: { authenticated: true } });
  act(() => result.current.open({ screenshot_id: 42 }));
  await waitFor(() => expect(mocks.existing.emit).toHaveBeenCalled());
  act(() => result.current.open({ screenshot_id: 42, window_title: 'Updated' }));
  expect(result.current.tabs).toHaveLength(1);
  expect(result.current.tabs[0].window_title).toBe('Updated');
  rerender({ authenticated: false });
  const count = mocks.existing.emit.mock.calls.length;
  await act(async () => { result.current.open({ screenshot_id: 99 }); await result.current.restore(); });
  expect(mocks.existing.emit).toHaveBeenCalledTimes(count);
  expect(result.current.tabs).toHaveLength(1);
});

it('routes window events to the current timeline handler without allowing locked navigation', async () => {
  const first = vi.fn(), second = vi.fn();
  const { rerender } = renderHook(({ authenticated, onOpenTimeline }) => useSnapshotPreviewWindow({ isAuthenticated: authenticated, onOpenTimeline }), {
    initialProps: { authenticated: true, onOpenTimeline: first },
  });
  await waitFor(() => expect(mocks.listeners.has('snapshot-preview-open-main')).toBe(true));
  rerender({ authenticated: true, onOpenTimeline: second });
  act(() => mocks.listeners.get('snapshot-preview-open-main')({ payload: { screenshot_id: 42 } }));
  expect(first).not.toHaveBeenCalled(); expect(second).toHaveBeenCalledOnce();
  rerender({ authenticated: false, onOpenTimeline: second });
  act(() => mocks.listeners.get('snapshot-preview-open-main')({ payload: { screenshot_id: 43 } }));
  expect(second).toHaveBeenCalledOnce();
});
