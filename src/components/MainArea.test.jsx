import React, { useState } from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import * as recap from '../lib/recap_api';
import MainArea from './MainArea';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key, i18n: { language: 'en' } }) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('./InspectorImage', () => ({ InspectorImage: () => null }));
vi.mock('./AdvancedSearch', () => ({ AdvancedSearch: () => null }));
vi.mock('./SmartClustersView', () => ({ default: () => null }));
vi.mock('./PreviewActionBar', () => ({ default: () => null }));
vi.mock('./OfficeDocumentEntry', () => ({ default: () => null }));
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
    fireEvent.click(await screen.findByRole('button', { name: activity.task_title }));
    fireEvent.click(screen.getByRole('button', { name: 'sidebar.recap.openDetails' }));
    expect((await screen.findByText('recap.snapshotActivity')).closest('article')).toHaveAttribute('data-recap-activity', activity.id);
    expect(recap.getRecapDay).toHaveBeenLastCalledWith(date, { includeRecords: false, includeAttempts: false });
    expect(select).not.toHaveBeenCalled();
    expect(screen.queryByRole('tablist')).not.toBeInTheDocument();
  });
});
