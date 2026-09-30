import React from 'react';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../../lib/auth_api';
import AiProvidersSection from './AiProvidersSection';

vi.mock('react-i18next', () => {
  const t = (key) => key;
  return { useTranslation: () => ({ t }) };
});
vi.mock('../../../lib/auth_api', () => ({ withAuth: vi.fn((action) => action()) }));

let provider;
beforeEach(() => {
  provider = { id: 'p', name: 'Local', kind: 'openai_compatible', base_url: 'http://localhost/v1', model: 'local', has_api_key: true };
  invoke.mockImplementation(async (command, args) => {
    if (command === 'ai_test_provider') return { ok: true, tool_calling: 'supported', latency_ms: 1 };
    if (command === 'ai_save_provider') provider = { ...provider, ...args.provider };
    return { providers: [provider], default_provider_id: 'p' };
  });
});

async function addPreset(id) {
  await screen.findByRole('button', { name: 'settings.ai.providers.edit Local' });
  fireEvent.click(screen.getByRole('button', { name: 'settings.ai.providers.add' }));
  fireEvent.change(screen.getByRole('combobox', { name: 'settings.ai.fields.preset' }), { target: { value: id } });
}

describe('simplified model services', () => {
  it.each([
    ['anthropic', 'claude-sonnet-5-5', 'https://api.anthropic.com', 'anthropic'],
    ['deepseek', 'deepseek-flash', 'https://api.deepseek.com/v1', 'openai_compatible'],
    ['qwen', 'qwen3.8-flash', 'https://dashscope.aliyuncs.com/compatible-mode/v1', 'openai_compatible'],
    ['kimi', 'kimi-k3', 'https://api.moonshot.cn/v1', 'openai_compatible'],
    ['glm', 'glm-5.3-flash', 'https://open.bigmodel.cn/api/paas/v4', 'openai_compatible'],
    ['mimo', 'mimo-v2.6-flash', 'https://api.xiaomimimo.com/v1', 'openai_compatible'],
    ['openai', 'gpt-6-luna', 'https://api.openai.com/v1', 'openai_compatible'],
    ['openrouter', 'deepseek/deepseek-v4.1-flash', 'https://openrouter.ai/api/v1', 'openai_compatible'],
  ])('tests and saves %s with only an API key and a 128k budget', async (id, model, baseUrl, kind) => {
    render(<AiProvidersSection />);
    await addPreset(id);
    expect(screen.getByLabelText('settings.ai.fields.base_url')).not.toBeVisible();
    expect(screen.getByLabelText('settings.ai.fields.model')).not.toBeVisible();
    expect(screen.getByLabelText('settings.ai.fields.name')).not.toBeVisible();
    expect(screen.getByLabelText('settings.ai.fields.kind')).not.toBeVisible();
    expect(screen.getByRole('button', { name: 'common.save' })).toBeDisabled();
    fireEvent.change(screen.getByLabelText('settings.ai.fields.api_key'), { target: { value: 'test-key' } });
    const expected = { provider: expect.objectContaining({ id: null, model, base_url: baseUrl, kind, api_key: 'test-key', context_tokens: 128000 }) };
    fireEvent.click(screen.getByRole('button', { name: 'settings.ai.test.button' }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('ai_test_provider', expected));
    await waitFor(() => expect(screen.getByRole('button', { name: 'common.save' })).toBeEnabled());
    fireEvent.click(screen.getByRole('button', { name: 'common.save' }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('ai_save_provider', expected));
    await screen.findByText('settings.ai.saved');
  });

  it('offers recommended, other and manual models in advanced settings', async () => {
    render(<AiProvidersSection />);
    await addPreset('glm');
    fireEvent.click(screen.getByText('settings.ai.advanced'));
    expect(within(screen.getByRole('group', { name: 'settings.ai.models.recommended' })).getByRole('option', { name: 'GLM-5.3-Flash' })).toBeInTheDocument();
    expect(within(screen.getByRole('group', { name: 'settings.ai.models.other' })).getByRole('option', { name: 'GLM-5.3' })).toBeInTheDocument();
    const model = screen.getByRole('combobox', { name: 'settings.ai.fields.model' });
    fireEvent.change(model, { target: { value: 'glm-5.3' } });
    expect(model).toHaveValue('glm-5.3');
    fireEvent.change(model, { target: { value: 'custom' } });
    const custom = screen.getByRole('textbox', { name: 'settings.ai.fields.model_id' });
    expect(custom).toHaveValue('glm-5.3');
    fireEvent.change(custom, { target: { value: 'my-model' } });
    fireEvent.change(screen.getByLabelText('settings.ai.fields.base_url'), { target: { value: 'https://example.com/v1' } });
    fireEvent.change(screen.getByLabelText('settings.ai.fields.context_tokens'), { target: { value: '64000' } });
    fireEvent.change(screen.getByLabelText('settings.ai.fields.api_key'), { target: { value: 'test-key' } });
    fireEvent.click(screen.getByRole('button', { name: 'common.save' }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('ai_save_provider', {
      provider: expect.objectContaining({ model: 'my-model', base_url: 'https://example.com/v1', context_tokens: 64000 }),
    }));
    await screen.findByText('settings.ai.saved');
  });

  it.each(['ollama', 'lmstudio'])('keeps %s model and address visible with a local budget and optional key', async (id) => {
    render(<AiProvidersSection />);
    await addPreset(id);
    expect(screen.getByLabelText('settings.ai.fields.base_url')).toBeVisible();
    expect(screen.getByLabelText('settings.ai.fields.model')).toBeVisible();
    fireEvent.change(screen.getByLabelText('settings.ai.fields.model'), { target: { value: 'installed-model' } });
    fireEvent.click(screen.getByRole('button', { name: 'common.save' }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('ai_save_provider', {
      provider: expect.objectContaining({ model: 'installed-model', context_tokens: 32768, api_key: null }),
    }));
    await screen.findByText('settings.ai.saved');
  });

  it('resets the model, key and budget when switching providers', async () => {
    render(<AiProvidersSection />);
    await addPreset('glm');
    fireEvent.change(screen.getByLabelText('settings.ai.fields.api_key'), { target: { value: 'glm-secret' } });
    fireEvent.change(screen.getByLabelText('settings.ai.fields.preset'), { target: { value: 'mimo' } });
    expect(screen.getByLabelText('settings.ai.fields.api_key')).toHaveValue('');
    expect(screen.getByLabelText('settings.ai.fields.model')).toHaveValue('mimo-v2.6-flash');
    expect(screen.getByRole('button', { name: 'common.save' })).toBeDisabled();
    fireEvent.change(screen.getByLabelText('settings.ai.fields.preset'), { target: { value: 'ollama' } });
    expect(screen.getByLabelText('settings.ai.fields.model')).toHaveValue('');
    expect(screen.getByLabelText('settings.ai.fields.context_tokens')).toHaveValue(32768);
    fireEvent.change(screen.getByLabelText('settings.ai.fields.preset'), { target: { value: 'custom' } });
    expect(screen.getByLabelText('settings.ai.fields.base_url')).toHaveValue('');
    expect(screen.getByLabelText('settings.ai.fields.kind')).toBeVisible();
  });

  it('preserves a saved cloud key, legacy model and custom budget when editing', async () => {
    Object.assign(provider, { name: 'My Kimi', base_url: 'https://api.moonshot.cn/v1/', model: 'kimi-k2-0905-preview', context_tokens: 64000 });
    render(<AiProvidersSection />);
    fireEvent.click(await screen.findByRole('button', { name: 'settings.ai.providers.edit My Kimi' }));
    expect(screen.getByLabelText('settings.ai.fields.base_url')).not.toBeVisible();
    expect(screen.getByRole('button', { name: 'common.save' })).toBeEnabled();
    fireEvent.click(screen.getByText('settings.ai.advanced'));
    expect(screen.getByLabelText('settings.ai.fields.model_id')).toHaveValue('kimi-k2-0905-preview');
    expect(screen.getByLabelText('settings.ai.fields.context_tokens')).toHaveValue(64000);
    fireEvent.change(screen.getByLabelText('settings.ai.fields.name'), { target: { value: 'Renamed' } });
    fireEvent.click(screen.getByRole('button', { name: 'common.save' }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('ai_save_provider', {
      provider: expect.objectContaining({ id: 'p', name: 'Renamed', model: 'kimi-k2-0905-preview', context_tokens: 64000, api_key: null }),
    }));
    await screen.findByText('settings.ai.saved');
  });
});

async function edit() {
  fireEvent.click(await screen.findByRole('button', { name: 'settings.ai.providers.edit Local' }));
  fireEvent.click(screen.getByText('settings.ai.advanced'));
  return screen.getByRole('spinbutton', { name: 'settings.ai.fields.context_tokens' });
}

describe('AI context budget settings', () => {
  it('defaults old providers, validates edits and saves through authenticated settings', async () => {
    const view = render(<AiProvidersSection />);
    const input = await edit();
    expect(input).toHaveValue(32768);
    for (const value of ['', '8191', '9000.5', '2000001']) {
      fireEvent.change(input, { target: { value } });
      expect(screen.getByRole('button', { name: 'common.save' })).toBeDisabled();
      expect(input).toHaveAttribute('aria-invalid', 'true');
    }
    fireEvent.change(input, { target: { value: '64000' } });
    fireEvent.click(screen.getByRole('button', { name: 'common.save' }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('ai_save_provider', {
      provider: expect.objectContaining({ id: 'p', context_tokens: 64000, api_key: null }),
    }));
    expect(withAuth).toHaveBeenCalledWith(expect.any(Function), { autoPrompt: true });
    await screen.findByText('settings.ai.saved');
    view.unmount();
    render(<AiProvidersSection />);
    expect(await edit()).toHaveValue(64000);
  });

  it('keeps each service budget when cancelling an edit', async () => {
    provider.context_tokens = 128000;
    render(<AiProvidersSection />);
    const input = await edit();
    expect(input).toHaveValue(128000);
    fireEvent.change(input, { target: { value: '16000' } });
    fireEvent.click(screen.getByRole('button', { name: 'common.cancel' }));
    expect(await edit()).toHaveValue(128000);
    expect(invoke.mock.calls.some(([command]) => command === 'ai_save_provider')).toBe(false);
  });
});
