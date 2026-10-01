import React from 'react';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { getRecapDay, getRecapSettings, saveRecapSettings } from '../../lib/recap_api';
import { openSettingsWindow } from '../../lib/settings_api';
import { SettingsActivityProvider, useSettingsWindowActivity } from './SettingsActivityContext';
import RecapSection from './RecapSection';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));
vi.mock('../../lib/recap_api', () => ({ getRecapSettings: vi.fn(), getRecapDay: vi.fn(), saveRecapSettings: vi.fn(), localDate: () => '2026-10-01', recapErrorKey: () => 'error' }));
vi.mock('../../lib/ai_api', () => ({ getAiSettings: vi.fn(async () => ({ providers: [{ id: 'p', name: 'Local model' }] })) }));
vi.mock('../../lib/settings_api', () => ({ openSettingsWindow: vi.fn(async () => {}) }));
let settings;
beforeEach(() => {
  settings = { enabled: false, language: 'zh-CN', provider_id: null, request_output_tokens: 32000, answer_tokens: 4000, batch_input_tokens: 36000, batch_output_tokens: 64000,
    daily_input_tokens: 432000, daily_output_tokens: 384000, screening_input_tokens: 2000000,
    screening: { enabled: false, base_url: 'https://example.com/v1', model: 'screen', has_api_key: true } };
  getRecapSettings.mockResolvedValue(settings);
  getRecapDay.mockResolvedValue({ usage: { input_tokens: 1, output_tokens: 2, screening_input_tokens: 3 } });
  saveRecapSettings.mockImplementation(async (value) => {
    const screening = { ...value.screening, has_api_key: Boolean(value.screening.api_key || value.screening.has_api_key) };
    delete screening.api_key;
    return { ...value, screening };
  });
});
function Activity() { const { dirty, busy } = useSettingsWindowActivity(); return <><span>{dirty ? 'dirty' : 'clean'}</span><span>{busy ? 'busy' : 'idle'}</span></>; }
function Host({ enabled = true }) { return <SettingsActivityProvider enabled={enabled}><Activity /><RecapSection /></SettingsActivityProvider>; }

describe('daily recap in the settings window', () => {
  it('still allows configuration when the usage summary cannot be read', async () => {
    getRecapDay.mockRejectedValueOnce(new Error('RECAP_INVALID_CACHE'));
    render(<Host />);
    expect(await screen.findByRole('switch', { name: 'recap.settings.enabled' })).toBeInTheDocument();
  });
  it('automatically saves switches without footer actions or a separate language setting', async () => {
    render(<Host />);
    const control = await screen.findByRole('switch', { name: 'recap.settings.enabled' });
    fireEvent.click(control);
    expect(screen.getByText('dirty')).toBeInTheDocument();
    await waitFor(() => expect(saveRecapSettings).toHaveBeenCalledWith(expect.objectContaining({ enabled: true })));
    await screen.findByText('clean');
    expect(screen.queryByRole('button', { name: 'common.cancel' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'recap.save' })).not.toBeInTheDocument();
    expect(screen.queryByRole('combobox', { name: 'recap.settings.language' })).not.toBeInTheDocument();
  });
  it('automatically saves the selected provider', async () => {
    render(<Host />);
    fireEvent.change(await screen.findByRole('combobox', { name: 'recap.settings.provider' }), { target: { value: 'p' } });
    await waitFor(() => expect(saveRecapSettings).toHaveBeenCalledWith(expect.objectContaining({ provider_id: 'p' })));
    await screen.findByText('clean');
  });
  it('keeps saved keys out of the editor and leaves an untouched key out of saves', async () => {
    render(<Host />);
    fireEvent.click(await screen.findByRole('switch', { name: 'recap.settings.screening' }));
    expect(screen.getByLabelText('recap.settings.apiKey')).toHaveValue('');
    await waitFor(() => expect(saveRecapSettings).toHaveBeenCalledWith(expect.objectContaining({ screening: expect.not.objectContaining({ api_key: expect.any(String) }) })));
    await screen.findByText('clean');
  });
  it('uses the existing model settings route', async () => {
    render(<Host />);
    fireEvent.click(await screen.findByRole('button', { name: 'recap.settings.manageModels' }));
    expect(openSettingsWindow).toHaveBeenCalledWith('ai');
  });
  it('clears drafts on lock and ignores a settings read that returns after locking', async () => {
    let resolve;
    getRecapSettings.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    const { rerender } = render(<Host />);
    rerender(<Host enabled={false} />);
    await act(async () => resolve(settings));
    expect(screen.queryByRole('switch')).not.toBeInTheDocument();
    expect(screen.getByText('clean')).toBeInTheDocument();
  });
  it('prevents saving a daily allowance smaller than a batch allowance', async () => {
    render(<Host />);
    const input = await screen.findByLabelText('recap.settings.daily_input_tokens');
    fireEvent.change(input, { target: { value: 4000 } });
    fireEvent.blur(input);
    expect(saveRecapSettings).not.toHaveBeenCalled();
    expect(screen.getByText('recap.errors.RECAP_INVALID_SETTINGS')).toBeInTheDocument();
    fireEvent.change(input, { target: { value: 500000 } });
    fireEvent.blur(input);
    await waitFor(() => expect(saveRecapSettings).toHaveBeenCalledWith(expect.objectContaining({ daily_input_tokens: 500000 })));
    await screen.findByText('clean');
  });
  it('saves numeric edits on blur and does not write unchanged values', async () => {
    render(<Host />);
    const input = await screen.findByLabelText('recap.settings.answer_tokens');
    fireEvent.blur(input);
    expect(saveRecapSettings).not.toHaveBeenCalled();
    fireEvent.change(input, { target: { value: '' } });
    fireEvent.blur(input);
    expect(saveRecapSettings).not.toHaveBeenCalled();
    fireEvent.change(input, { target: { value: '2048' } });
    expect(saveRecapSettings).not.toHaveBeenCalled();
    fireEvent.blur(input);
    await waitFor(() => expect(saveRecapSettings).toHaveBeenCalledWith(expect.objectContaining({ answer_tokens: 2048 })));
    await screen.findByText('clean');
  });
  it('saves text on Enter and clears the key editor after saving a new key', async () => {
    settings.screening.enabled = true;
    render(<Host />);
    const input = await screen.findByLabelText('recap.settings.apiKey');
    act(() => input.focus());
    fireEvent.change(input, { target: { value: 'new-test-key' } });
    expect(saveRecapSettings).not.toHaveBeenCalled();
    fireEvent.keyDown(input, { key: 'Enter' });
    await waitFor(() => expect(saveRecapSettings).toHaveBeenCalledWith(expect.objectContaining({ screening: expect.objectContaining({ api_key: 'new-test-key' }) })));
    await screen.findByText('clean');
    expect(input).toHaveValue('');
    expect(input).toHaveAttribute('placeholder', 'recap.settings.keySaved');
  });
  it('keeps incomplete screening fields local until they are valid', async () => {
    settings.screening.enabled = true;
    render(<Host />);
    const endpoint = await screen.findByLabelText('recap.settings.endpoint');
    fireEvent.change(endpoint, { target: { value: 'invalid-url' } });
    fireEvent.blur(endpoint);
    expect(saveRecapSettings).not.toHaveBeenCalled();
    expect(screen.getByText('recap.errors.RECAP_INVALID_SETTINGS')).toBeInTheDocument();
    fireEvent.change(endpoint, { target: { value: 'https://example.org/v1' } });
    const model = screen.getByLabelText('recap.settings.model');
    fireEvent.change(model, { target: { value: ' ' } });
    fireEvent.blur(model);
    expect(saveRecapSettings).not.toHaveBeenCalled();
    fireEvent.change(model, { target: { value: 'new-model' } });
    fireEvent.blur(model);
    await screen.findByText('clean');
    expect(saveRecapSettings).toHaveBeenCalledWith(expect.objectContaining({ screening: expect.objectContaining({ model: 'new-model', base_url: 'https://example.org/v1' }) }));
  });
  it('serializes rapid changes and preserves the latest selection', async () => {
    let resolve;
    saveRecapSettings.mockImplementationOnce((value) => new Promise((done) => { resolve = () => done(value); }));
    render(<Host />);
    const control = await screen.findByRole('switch', { name: 'recap.settings.enabled' });
    fireEvent.click(control);
    fireEvent.change(screen.getByRole('combobox', { name: 'recap.settings.provider' }), { target: { value: 'p' } });
    fireEvent.click(control);
    expect(saveRecapSettings).toHaveBeenCalledTimes(1);
    expect(screen.getByText('busy')).toBeInTheDocument();
    await act(async () => resolve());
    expect(saveRecapSettings).toHaveBeenCalledTimes(2);
    expect(saveRecapSettings).toHaveBeenLastCalledWith(expect.objectContaining({ enabled: false, provider_id: 'p' }));
    expect(control).not.toBeChecked();
    expect(screen.getByText('clean')).toBeInTheDocument();
    expect(screen.getByText('idle')).toBeInTheDocument();
  });
  it('preserves unfinished text edits when an earlier save completes', async () => {
    let resolve;
    saveRecapSettings.mockImplementationOnce((value) => new Promise((done) => { resolve = () => done(value); }));
    settings.screening.enabled = true;
    render(<Host />);
    fireEvent.click(await screen.findByRole('switch', { name: 'recap.settings.enabled' }));
    const model = screen.getByLabelText('recap.settings.model');
    fireEvent.change(model, { target: { value: 'still-editing' } });
    await act(async () => resolve());
    expect(model).toHaveValue('still-editing');
    expect(screen.getByText('dirty')).toBeInTheDocument();
    expect(saveRecapSettings).toHaveBeenCalledTimes(1);
    fireEvent.blur(model);
    await screen.findByText('clean');
    expect(saveRecapSettings).toHaveBeenLastCalledWith(expect.objectContaining({ enabled: true, screening: expect.objectContaining({ model: 'still-editing' }) }));
  });
  it('retains failed edits and offers retry without a save footer', async () => {
    saveRecapSettings.mockRejectedValueOnce(new Error('RECAP_SAVE_FAILED'));
    render(<Host />);
    const control = await screen.findByRole('switch', { name: 'recap.settings.enabled' });
    fireEvent.click(control);
    expect(await screen.findByRole('alert')).toHaveTextContent('error');
    expect(control).toBeChecked();
    expect(screen.getByText('dirty')).toBeInTheDocument();
    expect(screen.getByText('idle')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'common.retry' }));
    await screen.findByText('clean');
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    expect(saveRecapSettings).toHaveBeenCalledTimes(2);
  });
  it('drops queued edits and ignores a save response after locking', async () => {
    let resolve;
    saveRecapSettings.mockImplementationOnce((value) => new Promise((done) => { resolve = () => done(value); }));
    const { rerender } = render(<Host />);
    fireEvent.click(await screen.findByRole('switch', { name: 'recap.settings.enabled' }));
    fireEvent.change(screen.getByRole('combobox', { name: 'recap.settings.provider' }), { target: { value: 'p' } });
    rerender(<Host enabled={false} />);
    await act(async () => resolve());
    expect(screen.queryByRole('switch')).not.toBeInTheDocument();
    expect(screen.getByText('clean')).toBeInTheDocument();
    expect(screen.getByText('idle')).toBeInTheDocument();
    expect(saveRecapSettings).toHaveBeenCalledTimes(1);
  });
});
