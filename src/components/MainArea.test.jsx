import React, { useState } from 'react';
import { fireEvent, render, screen, within, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import * as recap from '../lib/recap_api';
import MainArea from './MainArea';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key, i18n: { language: 'en' } }) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('./InspectorImage', () => ({ InspectorImage: () => null }));
vi.mock('./AdvancedSearch', () => ({ AdvancedSearch: ({ onSelectResult }) => <button onClick={() => onSelectResult({ screenshot_id: 42, captured_at_ms: start })}>Open search snapshot</button> }));
vi.mock('./SmartClustersView', () => ({ default: () => null }));
vi.mock('./PreviewActionBar', () => ({ default: () => null }));
vi.mock('./OfficeDocumentEntry', () => ({ default: () => null }));
vi.mock('../lib/monitor_api', () => ({
  computeLinkScores: vi.fn(async () => []), updateScreenshotCategory: vi.fn(), resumeOfficeDocument: vi.fn(),
  fetchImage: vi.fn(async () => 'snapshot.png'), getScreenshotDetails: vi.fn(async () => ({ record: { id: 42 }, ocr_results: [{ text: 'sample text' }] })),
}));
vi.mock('../lib/recap_api', async (original) => ({
  ...await original(), getSnapshotRecapContext: vi.fn(), getRecapDay: vi.fn(), getRecapSettings: vi.fn(),
  listRecapDays: vi.fn(), getRecapProgress: vi.fn(async () => null),
}));

const date = '2026-01-01';
const start = Date.UTC(2026, 0, 1, 10, 0);
const source = { id: 42, timestamp_ms: start, process_name: 'Editor', window_title: 'Capture settings' };
const activity = { id: 'activity', task_id: 'task', task_title: 'Design recap associations', text: 'Compared association choices.',
  start_ms: start, end_ms: start, sources: [source], member_ids: [42] };
beforeEach(() => {
  localStorage.clear();
  recap.getRecapSettings.mockResolvedValue({ enabled: true });
  recap.listRecapDays.mockResolvedValue([date]);
  recap.getSnapshotRecapContext.mockResolvedValue({ status: 'ready', items: [{ ...activity, date, activity_id: activity.id }] });
  recap.getRecapDay.mockImplementation(async (requested) => ({ date: requested,
    batches: [{ start_ms: start, end_ms: start + 14400000, status: 'ready', activities: [activity], records: [source] }],
    threads: [{ id: 'task', title: activity.task_title, activity_ids: [activity.id] }], running: false, can_undo: false,
  }));
});

describe('main preview recap navigation', () => {
  it('opens the matched event detail without changing the selected snapshot', async () => {
    const select = vi.fn();
    function Preview() {
      const [tab, setTab] = useState('preview');
      return <MainArea activeTab={tab} setActiveTab={setTab} isAuthenticated selectedImageSrc="preview.png"
        selectedEvent={{ id: 42, timestamp: start, appName: 'Editor', windowTitle: 'Capture settings' }} onAdvancedSelect={select} />;
    }
    render(<Preview />);
    fireEvent.click(screen.getByRole('button', { name: 'snapshotPreview.details' }));
    fireEvent.click(await screen.findByRole('button', { name: activity.task_title }));
    fireEvent.click(screen.getByRole('button', { name: 'sidebar.recap.openDetails' }));
    expect((await screen.findByText('recap.snapshotActivity')).closest('article')).toHaveAttribute('data-recap-activity', activity.id);
    expect(recap.getRecapDay).toHaveBeenLastCalledWith(date, { includeRecords: false, includeAttempts: false });
    expect(select).not.toHaveBeenCalled();
    expect(screen.queryByRole('tablist', { name: 'recap.tabs' })).not.toBeInTheDocument();
  });

  it('previews a search snapshot without replacing the page and restores focus after Escape', async () => {
    const select = vi.fn();
    render(<MainArea activeTab="advanced-search" isAuthenticated onAdvancedSelect={select} />);
    const source = screen.getByRole('button', { name: 'Open search snapshot' });
    source.focus(); fireEvent.click(source);
    const dialog = await screen.findByRole('dialog', { name: 'snapshotPreview.windowTitle' });
    expect(within(dialog).getByRole('button', { name: 'snapshotPreview.openTimeline' })).toBeInTheDocument();
    expect(select).not.toHaveBeenCalled();
    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(document.activeElement).toBe(source);
    fireEvent.click(source);
    fireEvent.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'snapshotPreview.openTimeline' }));
    expect(select).toHaveBeenCalledWith(expect.objectContaining({ screenshot_id: 42, captured_at_ms: start }), { trigger: source });
  });
});
