import React from 'react';
import { act, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import SnapshotPreviewStandalone from './SnapshotPreviewStandalone';
import { SNAPSHOT_PREVIEW_WINDOW_STATE_KEY } from '../lib/snapshot_preview';

const mocks = vi.hoisted(() => ({ authenticated: true, titles: [], events: [], close: vi.fn(async () => {}), openMain: vi.fn(async () => {}) }));
vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key, i18n: { language: 'en' } }) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => vi.fn()), emitTo: mocks.openMain }));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => ({ close: mocks.close, startDragging: vi.fn(async () => {}) }) }));
vi.mock('../hooks/useAuthSession', () => ({ useAuthSession: () => ({ isAuthenticated: mocks.authenticated }) }));
vi.mock('./AuthMask', () => ({ default: ({ isVisible }) => isVisible ? <div>Locked preview</div> : null }));
vi.mock('../lib/monitor_api', () => ({
  computeLinkScores: vi.fn(async () => []), updateScreenshotCategory: vi.fn(), resumeOfficeDocument: vi.fn(),
  fetchImage: vi.fn(async () => 'snapshot.png'), getScreenshotDetails: vi.fn(async () => ({ record: { id: 42 }, ocr_results: [{ text: 'private text' }] })),
}));
vi.mock('../lib/recap_api', () => ({ getSnapshotRecapContext: vi.fn(async () => ({ status: 'unmatched', items: [] })) }));
vi.mock('./InspectorImage', () => ({ InspectorImage: () => <div>Original screenshot</div> }));

beforeEach(() => {
  localStorage.clear(); mocks.authenticated = true;
  localStorage.setItem(SNAPSHOT_PREVIEW_WINDOW_STATE_KEY, JSON.stringify({ tabs: [{ screenshot_id: 42, window_title: 'Private title' }], activeKey: 'id:42' }));
  invoke.mockResolvedValue(null);
});

it('hides the title, screenshot and text on lock and restores the selected tab on unlock', async () => {
  const { rerender } = render(<SnapshotPreviewStandalone />);
  await screen.findByText('Original screenshot');
  expect(document.title).toBe('Private title');
  mocks.authenticated = false;
  rerender(<SnapshotPreviewStandalone />);
  expect(screen.getByText('Locked preview')).toBeInTheDocument();
  expect(screen.queryByText('Original screenshot')).not.toBeInTheDocument();
  expect(screen.queryByText('Private title')).not.toBeInTheDocument();
  expect(document.title).toBe('snapshotPreview.windowTitle');
  mocks.authenticated = true;
  rerender(<SnapshotPreviewStandalone />);
  await screen.findByText('Original screenshot');
  expect(document.title).toBe('Private title');
});

it('uses an explicit timeline action to navigate from the native preview', async () => {
  render(<SnapshotPreviewStandalone />);
  await screen.findByText('Original screenshot');
  await act(async () => screen.getByRole('button', { name: 'snapshotPreview.openTimeline' }).click());
  await waitFor(() => expect(mocks.openMain).toHaveBeenCalledWith('main', 'snapshot-preview-open-main', expect.objectContaining({ screenshot_id: 42 })));
});
