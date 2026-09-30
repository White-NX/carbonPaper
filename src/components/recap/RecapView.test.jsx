import React from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { listen } from '@tauri-apps/api/event';
import * as api from '../../lib/recap_api';
import { grantAiRemoteConsent } from '../../lib/ai_api';
import RecapView from './RecapView';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key, i18n: { language: 'en' } }) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('../../lib/ai_api', () => ({ getAiSettings: vi.fn(async () => ({ providers: [{ id: 'p', name: 'Local model' }], remote_consent: false })), grantAiRemoteConsent: vi.fn(async () => ({ providers: [], remote_consent: true })) }));
vi.mock('../../lib/recap_api', async (original) => ({
  ...await original(), getRecapSettings: vi.fn(), getRecapDay: vi.fn(), getRecapProgress: vi.fn(), listRecapDays: vi.fn(async () => []),
  generateRecap: vi.fn(), cancelRecap: vi.fn(async () => true), correctRecap: vi.fn(), saveRecapSettings: vi.fn(),
}));

let day;
let settings;
beforeEach(() => {
  settings = { enabled: true, provider_id: null, language: 'en', batch_input_tokens: 36000, batch_output_tokens: 6000, daily_input_tokens: 216000, daily_output_tokens: 36000, screening_input_tokens: 2000000, screening: { enabled: false, base_url: 'https://api.typesafe.ai/v1', model: 'jev-1.13.0', has_api_key: true } };
  const sources = [{ id: 42, timestamp_ms: 1790467200000, process_name: 'Editor', window_title: 'Example document' }];
  day = { date: '2026-09-27', batches: [{ start_ms: sources[0].timestamp_ms, end_ms: sources[0].timestamp_ms + 14400000, status: 'ready', activities: [{ id: 'a1', task_id: 'task', task_title: 'Review login flow', text: 'Inspected the form validation.', start_ms: sources[0].timestamp_ms, end_ms: sources[0].timestamp_ms, sources }], records: sources }], threads: [{ id: 'task', title: 'Review login flow', activity_ids: ['a1'] }], running: false, can_undo: true, usage: { input_tokens: 12, output_tokens: 3, screening_input_tokens: 0 } };
  api.getRecapSettings.mockResolvedValue(settings);
  settings.request_output_tokens = 32000;
  settings.answer_tokens = 4000;
  api.getRecapProgress.mockResolvedValue(null);
  api.getRecapDay.mockResolvedValue(day);
  api.generateRecap.mockResolvedValue(day);
  api.correctRecap.mockResolvedValue(day);
  api.saveRecapSettings.mockImplementation(async (s) => s);
});

describe('daily recap page', () => {
  it('shows a specific failed-period reason and hides the empty welcome message', async () => {
    day.batches[0].status = 'failed';
    day.batches[0].error = 'RECAP_OUTPUT_TRUNCATED';
    day.batches[0].activities = [];
    render(<RecapView active isAuthenticated />);
    const alert = await screen.findByRole('alert');
    expect(within(alert).getByText('recap.errors.RECAP_OUTPUT_TRUNCATED')).toBeInTheDocument();
    expect(within(alert).getByText('RECAP_OUTPUT_TRUNCATED')).toBeInTheDocument();
    expect(screen.queryByText('recap.empty')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'recap.generate' }));
    await waitFor(() => expect(api.generateRecap).toHaveBeenCalledWith(expect.any(String), false));
  });
  it('explains partial failures while preserving usable activities', async () => {
    day.batches[0].status = 'partial';
    day.batches[0].error = 'RECAP_SCREENING_UNAVAILABLE';
    render(<RecapView active isAuthenticated />);
    expect(await screen.findByText('Inspected the form validation.')).toBeInTheDocument();
    expect(within(screen.getByRole('alert')).getByText('recap.errors.RECAP_SCREENING_UNAVAILABLE')).toBeInTheDocument();
  });
  it('shows errors from scheduled runs that failed before creating a batch', async () => {
    day.error = 'AI_NO_PROVIDER';
    render(<RecapView active isAuthenticated />);
    expect(await screen.findByText('recap.errors.AI_NO_PROVIDER')).toBeInTheDocument();
  });
  it('keeps an operation error through background refreshes until a successful retry', async () => {
    api.generateRecap.mockRejectedValueOnce(new Error('AI_BAD_REQUEST'));
    render(<RecapView active isAuthenticated />);
    await screen.findByText('Inspected the form validation.');
    fireEvent.click(screen.getByRole('button', { name: 'recap.generate' }));
    await screen.findByText('recap.errors.AI_BAD_REQUEST');
    const refresh = listen.mock.calls.find(([event]) => event === 'recap-changed')[1];
    await act(async () => { await refresh(); });
    expect(screen.getByText('recap.errors.AI_BAD_REQUEST')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'recap.generate' }));
    await waitFor(() => expect(api.generateRecap).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(screen.queryByRole('alert')).not.toBeInTheDocument());
  });
  it('opens cited sources through the existing preview path', async () => {
    const open = vi.fn(); const locate = vi.fn();
    render(<RecapView active isAuthenticated onOpenSnapshotPreview={open} onSelectScreenshot={locate} />);
    await screen.findByText('Inspected the form validation.');
    fireEvent.click(screen.getByRole('button', { name: /Editor/ }));
    expect(open).toHaveBeenCalledWith(expect.objectContaining({ screenshot_id: 42, timestamp: 1790467200 }), expect.objectContaining({ sourceType: 'recap' }));
    fireEvent.click(screen.getByRole('button', { name: 'recap.locate' }));
    expect(locate).toHaveBeenCalledWith(expect.objectContaining({ screenshot_id: 42 }));
  });
  it('saves a task rename and supports undo', async () => {
    render(<RecapView active isAuthenticated />);
    await screen.findByText('Inspected the form validation.');
    fireEvent.click(screen.getByRole('button', { name: 'recap.correct' }));
    const form = screen.getByRole('form', { name: 'recap.correct' });
    fireEvent.change(within(form).getByRole('textbox'), { target: { value: 'My login task' } });
    fireEvent.submit(form);
    await waitFor(() => expect(api.correctRecap).toHaveBeenCalledWith(expect.any(String), { kind: 'rename', task_id: 'task', title: 'My login task' }));
    await waitFor(() => expect(screen.getByRole('button', { name: 'recap.undo' })).not.toBeDisabled());
    fireEvent.click(screen.getByRole('button', { name: 'recap.undo' }));
    await waitFor(() => expect(api.correctRecap).toHaveBeenLastCalledWith(expect.any(String), { kind: 'undo' }));
  });
  it('shows remote consent for failures recorded in a batch and retries after consent', async () => {
    day.batches[0].status = 'failed'; day.batches[0].error = 'AI_REMOTE_CONSENT_REQUIRED'; day.batches[0].activities = [];
    render(<RecapView active isAuthenticated />);
    fireEvent.click(await screen.findByRole('button', { name: 'recap.allowRemote' }));
    await waitFor(() => expect(grantAiRemoteConsent).toHaveBeenCalledOnce());
    await waitFor(() => expect(api.generateRecap).toHaveBeenCalled());
  });
  it('does not place a saved screening key in the settings editor', async () => {
    render(<RecapView active isAuthenticated />);
    await screen.findByText('Inspected the form validation.');
    fireEvent.click(screen.getByRole('button', { name: 'recap.settings.title' }));
    fireEvent.click(screen.getByRole('checkbox', { name: 'recap.settings.screening' }));
    expect(screen.getByLabelText('recap.settings.apiKey')).toHaveValue('');
    fireEvent.submit(screen.getByRole('form', { name: 'recap.settings.title' }));
    await waitFor(() => expect(api.saveRecapSettings).toHaveBeenCalledWith(expect.objectContaining({ screening: expect.not.objectContaining({ api_key: expect.any(String) }) })));
  });
  it('clears private content when the session locks', async () => {
    const { rerender } = render(<RecapView active isAuthenticated />);
    await screen.findByText('Inspected the form validation.');
    rerender(<RecapView active isAuthenticated={false} />);
    expect(screen.queryByText('Inspected the form validation.')).not.toBeInTheDocument();
    expect(screen.getByText('recap.locked')).toBeInTheDocument();
  });
  it('stops a running background recap', async () => {
    day.running = true;
    render(<RecapView active isAuthenticated />);
    fireEvent.click(await screen.findByRole('button', { name: 'recap.stop' }));
    expect(api.cancelRecap).toHaveBeenCalledOnce();
  });
});
