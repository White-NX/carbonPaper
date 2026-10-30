import React from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { listen } from '@tauri-apps/api/event';
import * as api from '../../lib/recap_api';
import { grantAiRemoteConsent } from '../../lib/ai_api';
import { openSettingsWindow } from '../../lib/settings_api';
import RecapView from './RecapView';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key, i18n: { language: 'en' } }) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('../../lib/settings_api', () => ({ openSettingsWindow: vi.fn(async () => {}) }));
vi.mock('../../lib/ai_api', () => ({ getAiSettings: vi.fn(async () => ({ providers: [{ id: 'p', name: 'Local model' }], remote_consent: false })), grantAiRemoteConsent: vi.fn(async () => ({ providers: [], remote_consent: true })) }));
vi.mock('../../lib/recap_api', async (original) => ({
  ...await original(), getRecapSettings: vi.fn(), getRecapDay: vi.fn(), getRecapProgress: vi.fn(), getRecapRecords: vi.fn(), listRecapDays: vi.fn(async () => []),
  generateRecap: vi.fn(), deleteRecap: vi.fn(), cancelRecap: vi.fn(async () => true), correctRecap: vi.fn(), saveRecapSettings: vi.fn(),
}));

let day;
let settings;
beforeEach(() => {
  localStorage.clear();
  settings = { enabled: true, provider_id: null, language: 'en', batch_input_tokens: 36000, batch_output_tokens: 6000, daily_input_tokens: 216000, daily_output_tokens: 36000, screening_input_tokens: 2000000, screening: { enabled: false, base_url: 'https://api.typesafe.ai/v1', model: 'jev-1.13.0', has_api_key: true } };
  const sources = [{ id: 42, timestamp_ms: 1790467200000, process_name: 'Editor', window_title: 'Example document' }];
  day = { date: '2026-09-27', batches: [{ start_ms: sources[0].timestamp_ms, end_ms: sources[0].timestamp_ms + 14400000, status: 'ready', activities: [{ id: 'a1', task_id: 'task', task_title: 'Review login flow', text: 'Inspected the form validation.', start_ms: sources[0].timestamp_ms, end_ms: sources[0].timestamp_ms, sources }], records: sources }], threads: [{ id: 'task', title: 'Review login flow', activity_ids: ['a1'] }], running: false, can_undo: true, usage: { input_tokens: 12, output_tokens: 3, screening_input_tokens: 0 } };
  api.getRecapSettings.mockResolvedValue(settings);
  settings.request_output_tokens = 32000;
  settings.answer_tokens = 4000;
  api.getRecapProgress.mockResolvedValue(null);
  api.getRecapRecords.mockResolvedValue({ items: day.batches[0].records, total: 1, next_cursor: null });
  api.getRecapDay.mockResolvedValue(day);
  api.generateRecap.mockResolvedValue(day);
  api.correctRecap.mockResolvedValue(day);
  api.saveRecapSettings.mockImplementation(async (s) => s);
});

describe('daily recap page', () => {
  it('opens a snapshot deep link on a different date and scrolls to that occurrence', async () => {
    const original = day.batches[0].activities[0];
    day.batches[0].activities.push({ ...original, id: 'a2', start_ms: original.start_ms + 60000,
      end_ms: original.end_ms + 60000, sources: [{ ...original.sources[0], id: 43 }] });
    const rectangles = vi.spyOn(Element.prototype, 'getBoundingClientRect').mockImplementation(function () {
      const top = this.dataset?.recapActivity === 'a2' ? 500 : 20;
      return { top, bottom: top + 100, left: 0, right: 100, width: 100, height: 100, x: 0, y: top };
    });
    try {
      render(<RecapView active isAuthenticated navigation={{ date: day.date, task_id: 'task', activity_id: 'a2', screenshot_id: 43 }} />);
      const marker = await screen.findByText('recap.snapshotActivity');
      expect(marker.closest('article')).toHaveAttribute('data-recap-activity', 'a2');
      expect(document.getElementById('recap-panel-recap').scrollTop).toBe(468);
      expect(api.getRecapDay).toHaveBeenLastCalledWith(day.date, { includeRecords: false, includeAttempts: false });
      fireEvent.click(screen.getByRole('button', { name: 'recap.back' }));
      expect(screen.queryByText('recap.snapshotActivity')).not.toBeInTheDocument();
    } finally { rectangles.mockRestore(); }
  });

  it('opens a later deep link while the requested day is already loaded', async () => {
    const { rerender } = render(<RecapView active isAuthenticated navigation={{ date: day.date }} />);
    await screen.findByText('Inspected the form validation.');
    rerender(<RecapView active isAuthenticated navigation={{ date: day.date, task_id: 'task', activity_id: 'a1', screenshot_id: 42 }} />);
    expect((await screen.findByText('recap.snapshotActivity')).closest('article')).toHaveAttribute('data-recap-activity', 'a1');
    expect(screen.queryByRole('tablist')).not.toBeInTheDocument();
  });

  it('refreshes an inactive cached day before resolving a regenerated event', async () => {
    const navigation = { date: day.date };
    const { rerender } = render(<RecapView active isAuthenticated navigation={navigation} />);
    await screen.findByText('Inspected the form validation.');
    rerender(<RecapView active={false} isAuthenticated navigation={navigation} />);
    day = { ...day, batches: [{ ...day.batches[0], activities: [{ ...day.batches[0].activities[0], id: 'fresh', task_id: 'new-task', task_title: 'Regenerated event' }] }],
      threads: [{ id: 'new-task', title: 'Regenerated event', activity_ids: ['fresh'] }] };
    api.getRecapDay.mockResolvedValue(day);
    rerender(<RecapView active isAuthenticated navigation={{ date: day.date, task_id: 'new-task', activity_id: 'fresh', screenshot_id: 42 }} />);
    expect((await screen.findByText('recap.snapshotActivity')).closest('article')).toHaveAttribute('data-recap-activity', 'fresh');
  });

  it('rebinds regenerated IDs through the selected snapshot and reports a removed association', async () => {
    day.batches[0].activities[0] = { ...day.batches[0].activities[0], id: 'regenerated', task_id: 'corrected', task_title: 'Corrected event' };
    day.threads = [{ id: 'corrected', title: 'Corrected event', activity_ids: ['regenerated'] }];
    const { rerender } = render(<RecapView active isAuthenticated navigation={{ date: day.date, task_id: 'task', activity_id: 'a1', screenshot_id: 42 }} />);
    expect((await screen.findByText('recap.snapshotActivity')).closest('article')).toHaveAttribute('data-recap-activity', 'regenerated');
    rerender(<RecapView active isAuthenticated navigation={{ date: day.date, task_id: 'corrected', activity_id: 'regenerated', screenshot_id: 999 }} />);
    expect(await screen.findByText('recap.eventUnavailable')).toBeInTheDocument();
    expect(screen.queryByText('recap.snapshotActivity')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'recap.undo' })).not.toBeInTheDocument();
  });

  it('uses the real settings destination for the initial page', async () => {
    settings.enabled = false;
    render(<RecapView active isAuthenticated />);
    fireEvent.click(await screen.findByRole('button', { name: 'recap.configure' }));
    expect(openSettingsWindow).toHaveBeenCalledWith('organize', 'daily-recap');
    expect(screen.queryByRole('form')).not.toBeInTheDocument();
    expect(screen.queryByText('Inspected the form validation.')).not.toBeInTheDocument();
  });
  it('distinguishes waiting for a closed period from a day ready to generate', async () => {
    day.batches = [];
    render(<RecapView active isAuthenticated />);
    expect(await screen.findByText('recap.waitingDescription')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'recap.generateFirst' })).not.toBeInTheDocument();
  });
  const enterEvent = async () => fireEvent.click(await screen.findByRole('button', { name: /Review login flow/ }));
  const more = () => fireEvent.click(screen.getByRole('button', { name: 'recap.more' }));

  it('keeps live progress in the header and opens details without refreshing the reading surface', async () => {
    day.batches[0].updated_at_ms = day.batches[0].start_ms;
    const progress = { run_id: 'run', version: 1, stage: 'writing', completed_batches: 3, total_batches: 4, finished_at_ms: null };
    api.getRecapProgress.mockResolvedValue(progress);
    render(<RecapView active isAuthenticated />);
    await screen.findByText('Inspected the form validation.');
    const header = within(document.querySelector('header'));
    const status = await header.findByRole('status');
    expect(status).toHaveTextContent('recap.runStatus.organizing · 3/4');
    expect(header.getByRole('progressbar')).toHaveAttribute('aria-valuenow', '3');
    expect(header.getByRole('progressbar')).toHaveAttribute('aria-valuemax', '4');
    expect(screen.queryByText('recap.updated')).not.toBeInTheDocument();
    expect(api.getRecapProgress).toHaveBeenCalledWith(api.localDate(), { includeAttempts: false });
    const dayReads = api.getRecapDay.mock.calls.length;
    const notify = listen.mock.calls.find(([event]) => event === 'recap-progress')[1];
    api.getRecapProgress.mockResolvedValue({ ...progress, version: 2, stage: 'summarizing' });
    await act(async () => { await notify(); });
    expect(api.getRecapDay).toHaveBeenCalledTimes(dayReads);
    fireEvent.click(within(status).getByRole('button'));
    expect(await screen.findByRole('dialog')).toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: 'recap.close' }));
    await enterEvent();
    expect(header.getByRole('status')).toBe(status);
    expect(screen.queryByRole('tablist')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'recap.back' }));
    fireEvent.click(screen.getByRole('button', { name: 'recap.stop' }));
    expect(api.cancelRecap).toHaveBeenCalledOnce();
  });

  it('shows finishing until the run completes, then restores the update time and refreshes the day', async () => {
    day.batches[0].updated_at_ms = day.batches[0].start_ms;
    const progress = { run_id: 'run', version: 1, stage: 'writing', completed_batches: 3, total_batches: 4, finished_at_ms: null };
    api.getRecapProgress.mockResolvedValue(progress);
    render(<RecapView active isAuthenticated />);
    await screen.findByText('Inspected the form validation.');
    const header = within(document.querySelector('header'));
    await header.findByRole('status');
    const dayReads = api.getRecapDay.mock.calls.length;
    const notify = listen.mock.calls.find(([event]) => event === 'recap-progress')[1];
    api.getRecapProgress.mockResolvedValue({ ...progress, version: 2, stage: 'saving', completed_batches: 4 });
    await act(async () => { await notify(); });
    expect(header.getByRole('status')).toHaveTextContent('recap.runStatus.finishing · 4/4');
    expect(header.getByRole('progressbar')).not.toHaveAttribute('aria-valuenow');
    expect(api.getRecapDay).toHaveBeenCalledTimes(dayReads);
    api.getRecapProgress.mockResolvedValue({ ...progress, version: 3, stage: 'ready', completed_batches: 4, finished_at_ms: Date.now() });
    await act(async () => { await notify(); });
    await waitFor(() => expect(api.getRecapDay).toHaveBeenCalledTimes(dayReads + 1));
    expect(header.getByTitle('recap.updated')).toHaveAttribute('type', 'date');
    expect(header.queryByRole('status')).not.toBeInTheDocument();
    expect(header.queryByRole('progressbar')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'recap.stop' })).not.toBeInTheDocument();
  });

  it('handles unknown totals and keeps the pause reason available with counted progress', async () => {
    const progress = { run_id: 'run', version: 1, stage: 'preparing', completed_batches: 0, total_batches: 0, finished_at_ms: null };
    api.getRecapProgress.mockResolvedValue(progress);
    render(<RecapView active isAuthenticated />);
    const header = within(document.querySelector('header'));
    const status = await header.findByRole('status');
    expect(status).toHaveTextContent('recap.runStatus.organizing');
    expect(header.getByRole('progressbar')).not.toHaveAttribute('aria-valuenow');
    expect(status).not.toHaveTextContent('0/0');
    const notify = listen.mock.calls.find(([event]) => event === 'recap-progress')[1];
    api.getRecapProgress.mockResolvedValue({ ...progress, version: 2, stage: 'paused', completed_batches: 3, total_batches: 4 });
    await act(async () => { await notify(); });
    expect(header.getByRole('status')).toHaveTextContent('recap.runStatus.paused · 3/4');
    expect(header.getByRole('progressbar')).toHaveAttribute('aria-valuenow', '3');
    expect(within(header.getByRole('status')).getByRole('button')).toHaveAttribute('title', expect.stringContaining('recap.progress.stages.paused'));
  });

  it('keeps progress read errors compact and allows opening generation details', async () => {
    api.getRecapProgress.mockRejectedValueOnce(new Error('AI_BUSY'));
    render(<RecapView active isAuthenticated />);
    const header = within(document.querySelector('header'));
    const alert = await header.findByRole('alert');
    expect(alert).toHaveTextContent('recap.runStatus.unavailable');
    expect(within(alert).getByRole('button')).toHaveAttribute('title', 'recap.errors.AI_BUSY');
    fireEvent.click(within(alert).getByRole('button'));
    expect(await screen.findByRole('dialog')).toBeVisible();
  });

  it('confirms deletion with the shared dialog and refreshes the day and date list', async () => {
    api.listRecapDays.mockResolvedValue([api.localDate()]);
    render(<RecapView active isAuthenticated />);
    await screen.findByText('Inspected the form validation.');
    more();
    fireEvent.click(screen.getByRole('menuitem', { name: 'recap.delete' }));
    expect(screen.getByRole('dialog', { name: 'recap.deleteTitle' })).toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: 'common.cancel' }));
    expect(api.deleteRecap).not.toHaveBeenCalled();
    more();
    fireEvent.click(screen.getByRole('menuitem', { name: 'recap.delete' }));
    api.deleteRecap.mockImplementationOnce(async () => {
      api.getRecapDay.mockResolvedValue({ ...day, batches: [], threads: [], can_undo: false });
      api.listRecapDays.mockResolvedValue([]);
    });
    fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'recap.delete' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(api.deleteRecap).toHaveBeenCalledWith(api.localDate());
    expect(await screen.findByText('recap.waitingDescription')).toBeVisible();
    expect(document.querySelectorAll('#recap-dates option')).toHaveLength(0);
    expect(screen.queryByText('Inspected the form validation.')).not.toBeInTheDocument();
  });

  it('retains the recap when deletion fails and disables deletion during generation', async () => {
    api.deleteRecap.mockRejectedValueOnce(new Error('AI_BUSY'));
    const { rerender } = render(<RecapView active isAuthenticated />);
    await screen.findByText('Inspected the form validation.');
    more();
    fireEvent.click(screen.getByRole('menuitem', { name: 'recap.delete' }));
    fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'recap.delete' }));
    expect(await screen.findByText('recap.errors.AI_BUSY')).toBeVisible();
    expect(screen.getByText('Inspected the form validation.')).toBeVisible();
    day.running = true;
    rerender(<RecapView active isAuthenticated />);
    more();
    expect(screen.getByRole('menuitem', { name: 'recap.delete' })).toBeDisabled();
  });

  it('shows the period overview and opens a full event history without hidden source nodes', async () => {
    const batch = day.batches[0];
    batch.activities.push({ ...batch.activities[0], id: 'a2', text: 'Returned to inspect the output.', start_ms: batch.start_ms + 600000 });
    batch.summary = { overview: 'Reviewed validation and results.', topics: [{ task_id: 'task', title: 'Login validation', text: 'Checked the form and report.' }] };
    render(<RecapView active isAuthenticated />);
    expect(await screen.findByText(batch.summary.overview)).toBeVisible();
    expect(screen.queryByText('Returned to inspect the output.')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Editor/ })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: /Login validation/ }));
    expect(screen.queryByText(batch.summary.overview)).not.toBeInTheDocument();
    expect(screen.getByText('Returned to inspect the output.')).toBeVisible();
    expect(screen.getByRole('button', { name: 'recap.back' })).toBeVisible();
    expect(api.getRecapDay).toHaveBeenCalledWith(expect.any(String), { includeRecords: false, includeAttempts: false });
  });

  it('restores the overview position and focus, including after leaving for preview', async () => {
    const { rerender } = render(<RecapView active isAuthenticated />);
    const entry = await screen.findByRole('button', { name: /Review login flow/ });
    const panel = screen.getByRole('tabpanel');
    panel.scrollTop = 640;
    fireEvent.scroll(panel);
    fireEvent.click(entry);
    const detail = document.getElementById('recap-panel-recap');
    detail.scrollTop = 320;
    fireEvent.scroll(detail);
    rerender(<RecapView active={false} isAuthenticated />);
    expect(screen.queryByText('Inspected the form validation.')).not.toBeInTheDocument();
    await act(async () => rerender(<RecapView active isAuthenticated />));
    expect(screen.getByRole('button', { name: 'recap.back' })).toBeInTheDocument();
    expect(document.getElementById('recap-panel-recap').scrollTop).toBe(320);
    fireEvent.click(screen.getByRole('button', { name: 'recap.back' }));
    expect(screen.getByRole('tabpanel').scrollTop).toBe(640);
    expect(screen.getByRole('button', { name: /Review login flow/ })).toHaveFocus();
  });

  it('shows apps with a lightweight day response and removes them on lock', async () => {
    day.batches[0].records = [];
    day.batches[0].apps = [{ name: 'Editor', icon: null }, { name: 'Terminal', icon: null }];
    const { rerender } = render(<RecapView active isAuthenticated />);
    const apps = await screen.findByRole('group', { name: 'recap.apps' });
    expect(within(apps).getByRole('img', { name: 'Terminal' })).toBeVisible();
    expect(api.getRecapRecords).not.toHaveBeenCalled();
    rerender(<RecapView active isAuthenticated={false} />);
    expect(screen.queryByRole('group', { name: 'recap.apps' })).not.toBeInTheDocument();
  });

  it('keeps cached activity usable when summarization fails', async () => {
    day.batches[0].summary_error = 'RECAP_INVALID_SUMMARY';
    render(<RecapView active isAuthenticated />);
    expect(await screen.findByText('recap.errors.RECAP_INVALID_SUMMARY')).toBeInTheDocument();
    await enterEvent();
    expect(screen.getByText('Inspected the form validation.')).toBeVisible();
  });

  it('keeps failed-period diagnostics behind the details dialog', async () => {
    Object.assign(day.batches[0], { status: 'failed', error: 'RECAP_OUTPUT_TRUNCATED', activities: [] });
    render(<RecapView active isAuthenticated />);
    expect(await screen.findByText('recap.errors.RECAP_OUTPUT_TRUNCATED')).toBeVisible();
    expect(screen.queryByText('RECAP_OUTPUT_TRUNCATED')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'recap.generationDetails' }));
    fireEvent.click(await within(screen.getByRole('dialog')).findByRole('button', { name: 'recap.errorDetails' }));
    expect(await within(screen.getByRole('dialog')).findByText('RECAP_OUTPUT_TRUNCATED')).toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: 'recap.close' }));
    fireEvent.click(screen.getByRole('button', { name: 'recap.generate' }));
    await waitFor(() => expect(api.generateRecap).toHaveBeenCalledWith(expect.any(String), false));
  });

  it('preserves usable activities for partial failure and offers completion', async () => {
    Object.assign(day.batches[0], { status: 'partial', error: 'RECAP_SCREENING_UNAVAILABLE' });
    render(<RecapView active isAuthenticated />);
    expect(await screen.findByText('Inspected the form validation.')).toBeInTheDocument();
    expect(screen.getByText('recap.errors.RECAP_SCREENING_UNAVAILABLE')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'recap.generate' }));
    await waitFor(() => expect(api.generateRecap).toHaveBeenCalled());
  });

  it('shows failures before the first batch and retains operation errors through refreshes', async () => {
    day.error = 'AI_NO_PROVIDER';
    render(<RecapView active isAuthenticated />);
    expect(await screen.findByText('recap.errors.AI_NO_PROVIDER')).toBeInTheDocument();
    day.error = null;
    api.generateRecap.mockRejectedValueOnce(new Error('AI_BAD_REQUEST'));
    fireEvent.click(screen.getByRole('button', { name: 'recap.generate' }));
    await screen.findByText('recap.errors.AI_BAD_REQUEST');
    const refresh = listen.mock.calls.find(([event]) => event === 'recap-changed')[1];
    await act(async () => { await refresh(); });
    expect(screen.getByText('recap.errors.AI_BAD_REQUEST')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'recap.generate' }));
    await waitFor(() => expect(screen.queryByText('recap.errors.AI_BAD_REQUEST')).not.toBeInTheDocument());
  });

  it.each(['standalone', 'preview'])('honors the %s screenshot preference and alternate action', async (behavior) => {
    localStorage.setItem('cardClickBehavior_recap', behavior);
    const open = vi.fn(); const locate = vi.fn();
    render(<RecapView active isAuthenticated onOpenSnapshotPreview={open} onSelectScreenshot={locate} />);
    await enterEvent();
    fireEvent.click(screen.getByRole('button', { name: /Editor/ }));
    const primary = behavior === 'standalone' ? open : locate;
    expect(primary).toHaveBeenCalledWith(expect.objectContaining({ screenshot_id: 42 }), ...(behavior === 'standalone' ? [expect.objectContaining({ sourceType: 'recap' })] : []));
    fireEvent.click(screen.getByRole('button', { name: behavior === 'standalone' ? 'previewAction.openMainPreview' : 'previewAction.openFloatingPreview' }));
    expect(open).toHaveBeenCalledOnce();
    expect(locate).toHaveBeenCalledOnce();
  });

  it('saves a rename in a focused dialog, retains the event and supports undo', async () => {
    render(<RecapView active isAuthenticated />);
    await enterEvent();
    fireEvent.click(screen.getByRole('button', { name: 'recap.editEvent' }));
    fireEvent.click(screen.getByRole('menuitem', { name: 'recap.correction.rename' }));
    const dialog = screen.getByRole('dialog');
    fireEvent.change(within(dialog).getByRole('textbox'), { target: { value: 'My login task' } });
    fireEvent.submit(within(dialog).getByRole('form'));
    await waitFor(() => expect(api.correctRecap).toHaveBeenCalledWith(expect.any(String), { kind: 'rename', task_id: 'task', title: 'My login task' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(screen.getByRole('button', { name: 'recap.back' })).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'recap.undo' }));
    await waitFor(() => expect(api.correctRecap).toHaveBeenLastCalledWith(expect.any(String), { kind: 'undo' }));
  });

  it('edits an occurrence locally and Escape closes only the dialog', async () => {
    render(<RecapView active isAuthenticated />);
    await enterEvent();
    fireEvent.click(screen.getByRole('button', { name: 'recap.adjust' }));
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'recap.back' })).toBeInTheDocument();
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(screen.queryByRole('button', { name: 'recap.back' })).not.toBeInTheDocument();
  });

  it('follows the merge destination while preserving a way back to the day', async () => {
    day.threads.push({ id: 'target', title: 'Project work' });
    render(<RecapView active isAuthenticated />);
    await enterEvent();
    fireEvent.click(screen.getByRole('button', { name: 'recap.editEvent' }));
    fireEvent.click(screen.getByRole('menuitem', { name: 'recap.correction.merge' }));
    fireEvent.change(within(screen.getByRole('dialog')).getByRole('combobox'), { target: { value: 'target' } });
    fireEvent.submit(screen.getByRole('form'));
    await waitFor(() => expect(api.correctRecap).toHaveBeenCalledWith(expect.any(String), { kind: 'merge', from: 'task', into: 'target' }));
    expect(await screen.findByRole('heading', { name: 'Project work' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'recap.back' })).toBeInTheDocument();
  });

  it('loads raw records only after selecting their tab', async () => {
    render(<RecapView active isAuthenticated />);
    await screen.findByText('Inspected the form validation.');
    expect(api.getRecapRecords).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('tab', { name: 'recap.records' }));
    expect(await screen.findByText('Example document')).toBeVisible();
    expect(api.getRecapRecords).toHaveBeenCalledWith(expect.any(String), null, null);
    expect(screen.queryByText('Inspected the form validation.')).not.toBeInTheDocument();
  });

  it('requires remote consent before retrying and stops a background recap', async () => {
    Object.assign(day.batches[0], { status: 'failed', error: 'AI_REMOTE_CONSENT_REQUIRED', activities: [] });
    day.running = true;
    render(<RecapView active isAuthenticated />);
    fireEvent.click(await screen.findByRole('button', { name: 'recap.stop' }));
    expect(api.cancelRecap).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole('button', { name: 'recap.allowRemote' }));
    await waitFor(() => expect(grantAiRemoteConsent).toHaveBeenCalledOnce());
    await waitFor(() => expect(api.generateRecap).toHaveBeenCalled());
  });

  it('clears private content and detail state on lock', async () => {
    const { rerender } = render(<RecapView active isAuthenticated />);
    await enterEvent();
    rerender(<RecapView active isAuthenticated={false} />);
    expect(screen.queryByText('Inspected the form validation.')).not.toBeInTheDocument();
    expect(screen.getByText('recap.locked')).toBeInTheDocument();
    rerender(<RecapView active isAuthenticated />);
    await screen.findByText('Inspected the form validation.');
    expect(screen.queryByRole('button', { name: 'recap.back' })).not.toBeInTheDocument();
  });

  it('keeps logs off the reading surface until explicitly requested', async () => {
    day.batches[0].attempts = [{ id: 'attempt', model: 'model', kind: 'initial', status: 'ready', reasoning: 'Private reasoning', text: '', reasoning_chars: 17, text_chars: 0, started_at_ms: 1, finished_at_ms: 2 }];
    render(<RecapView active isAuthenticated />);
    await screen.findByText('Inspected the form validation.');
    expect(screen.queryByText('Private reasoning')).not.toBeInTheDocument();
    more();
    fireEvent.click(screen.getByRole('menuitem', { name: 'recap.generationDetails' }));
    const dialog = within(await screen.findByRole('dialog'));
    fireEvent.click(await dialog.findByRole('button', { name: /recap.progress.attempts.initial/ }));
    expect(screen.queryByText('Private reasoning')).not.toBeInTheDocument();
    fireEvent.click(dialog.getByRole('tab', { name: 'recap.diagnostics.tabs.reasoning' }));
    expect(await screen.findByText('Private reasoning')).toBeInTheDocument();
  });
});
