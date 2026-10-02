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
  generateRecap: vi.fn(), cancelRecap: vi.fn(async () => true), correctRecap: vi.fn(), saveRecapSettings: vi.fn(),
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
    expect(await screen.findByText('Private reasoning')).toBeInTheDocument();
  });
});
