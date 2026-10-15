import React from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import * as api from '../../lib/recap_api';
import RecapSearch from './RecapSearch';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key, i18n: { language: 'en' } }) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('../../lib/recap_api', async (original) => ({ ...await original(), searchRecaps: vi.fn(), getRecapSearchStatus: vi.fn(), prepareRecapSearch: vi.fn(), cancelRecapSearchIndex: vi.fn() }));
vi.mock('./RecapSources', async (original) => ({ ...await original(), SourceThumbnail: ({ source, action }) => <button onClick={() => action.open(source)}>{source.window_title}</button> }));
const source = { id: 42, timestamp_ms: 1791507600000, process_name: 'Editor', window_title: 'Source notes' };
const hit = { date: '2026-10-09', activity_id: 'a', task_id: 'task', task_title: 'Compare retrieval approaches', text: 'Read a comparison of OCR and recaps.', start_ms: source.timestamp_ms, end_ms: source.timestamp_ms,
  sources: [source], details: [{ text: 'Keep individual source citations.', evidence_ids: [42] }, { text: 'Unsupported hidden fact', evidence_ids: [99] }] };
const props = () => ({ active: true, authenticated: true, action: { open: vi.fn(), floating: true }, time: () => '09:00', onOpenDay: vi.fn() });
beforeEach(() => {
  vi.clearAllMocks();
  api.getRecapSearchStatus.mockResolvedValue({ revision: 'v1', activities: 2, dates: 2, indexed_chunks: 4, total_chunks: 4, running: false });
  api.searchRecaps.mockResolvedValue({ revision: 'v1', items: [hit], mode: 'hybrid', semantic_status: 'ready', next_offset: null, total: 1, elapsed_ms: 8, indexed_chunks: 4, total_chunks: 4 });
});
const search = () => {
  fireEvent.change(screen.getByRole('searchbox'), { target: { value: 'retrieval' } });
  fireEvent.submit(screen.getByRole('form', { name: 'recap.search.form' }));
};

describe('recap search interface', () => {
  it('opens cited details and returns to the same search', async () => {
    const actions = props();
    render(<RecapSearch {...actions} />);
    search();
    fireEvent.click(await screen.findByRole('button', { name: /Compare retrieval approaches/ }));
    const dialog = screen.getByRole('dialog', { name: 'recap.search.activity' });
    expect(within(dialog).getByText('Keep individual source citations.')).toBeVisible();
    expect(within(dialog).queryByText('Unsupported hidden fact')).not.toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole('button', { name: 'Source notes' }));
    expect(actions.action.open).toHaveBeenCalledWith(source);
    fireEvent.click(within(dialog).getByRole('button', { name: 'recap.detailSources' }));
    expect(screen.getAllByRole('dialog', { hidden: true })).toHaveLength(2);
    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'recap.sources' })).not.toBeInTheDocument());
    fireEvent.click(within(dialog).getByRole('button', { name: 'recap.close' }));
    expect(screen.getByRole('searchbox')).toHaveValue('retrieval');
    expect(screen.getByRole('button', { name: /Compare retrieval approaches/ })).toBeVisible();
  });

  it('supports controlled comparison and rejects an inverted date range', async () => {
    render(<RecapSearch {...props()} />);
    fireEvent.click(screen.getByRole('button', { name: 'recap.search.modes.keyword' }));
    fireEvent.click(screen.getByRole('button', { name: 'recap.search.options' }));
    fireEvent.click(screen.getByRole('checkbox'));
    fireEvent.change(screen.getByRole('combobox'), { target: { value: 'custom' } });
    fireEvent.change(screen.getByLabelText('recap.search.startDate'), { target: { value: '2026-10-10' } });
    fireEvent.change(screen.getByLabelText('recap.search.endDate'), { target: { value: '2026-10-01' } });
    fireEvent.change(screen.getByRole('searchbox'), { target: { value: 'retrieval' } });
    expect(screen.getByRole('button', { name: 'recap.search.submit' })).toBeDisabled();
    fireEvent.change(screen.getByLabelText('recap.search.endDate'), { target: { value: '2026-10-15' } });
    fireEvent.submit(screen.getByRole('form'));
    await waitFor(() => expect(api.searchRecaps).toHaveBeenCalledWith(expect.objectContaining({ mode: 'keyword', include_details: false, start_date: '2026-10-10', end_date: '2026-10-15' })));
  });

  it('distinguishes keyword fallback from complete meaning search', async () => {
    api.getRecapSearchStatus.mockResolvedValue({ revision: 'v1', activities: 2, dates: 2, indexed_chunks: 0, total_chunks: 4, running: false });
    api.searchRecaps.mockResolvedValue({ revision: 'v1', items: [], mode: 'hybrid', semantic_status: 'not_prepared', total: 0, next_offset: null });
    render(<RecapSearch {...props()} />);
    expect(await screen.findByRole('button', { name: 'recap.search.prepare' })).toBeVisible();
    search();
    expect(await screen.findByText('recap.search.keywordFallback')).toBeVisible();
    expect(screen.getByText('recap.search.emptyHint')).toBeVisible();
  });

  it('clears result details when authentication is lost', async () => {
    const actions = props();
    const { rerender } = render(<RecapSearch {...actions} />);
    search();
    fireEvent.click(await screen.findByRole('button', { name: /Compare retrieval approaches/ }));
    rerender(<RecapSearch {...actions} authenticated={false} />);
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(screen.queryByText('Read a comparison of OCR and recaps.')).not.toBeInTheDocument();
  });
});
