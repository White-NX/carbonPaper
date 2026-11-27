import React from 'react';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
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
    if (command === 'ai_get_model_catalog') return { providers: [] };
    if (command === 'ai_discover_models') return { models: [], stale: true };
    if (command === 'ai_test_image_input') return { image_input: 'supported', latency_ms: 1 };
    if (command === 'ai_test_provider') return { ok: true, tool_calling: 'supported', latency_ms: 1 };
    if (command === 'ai_save_provider') provider = { ...provider, ...args.provider };
    return { providers: [provider], default_provider_id: 'p' };
  });
});

function selectOption(label, value) {
  fireEvent.click(screen.getByRole('combobox', { name: label }));
  fireEvent.click(screen.getAllByRole('option').find((option) => option.value === value));
}

async function addPreset(id) {
  await screen.findByRole('button', { name: 'settings.ai.providers.edit Local' });
  fireEvent.click(screen.getByRole('button', { name: 'settings.ai.providers.add' }));
  selectOption('settings.ai.fields.preset', id);
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
  ])('tests and saves %s with only an API key and automatic model context', async (id, model, baseUrl, kind) => {
    render(<AiProvidersSection />);
    await addPreset(id);
    expect(screen.getByLabelText('settings.ai.fields.base_url')).not.toBeVisible();
    expect(screen.getByLabelText('settings.ai.fields.model')).not.toBeVisible();
    expect(screen.getByLabelText('settings.ai.fields.name')).not.toBeVisible();
    expect(screen.getByLabelText('settings.ai.fields.kind')).not.toBeVisible();
    expect(screen.getByRole('button', { name: 'common.save' })).toBeDisabled();
    fireEvent.change(screen.getByLabelText('settings.ai.fields.api_key'), { target: { value: 'test-key' } });
    const expected = { provider: expect.objectContaining({ id: null, model, base_url: baseUrl, kind, api_key: 'test-key', context_tokens: null }) };
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
    const model = screen.getByRole('combobox', { name: 'settings.ai.fields.model' });
    fireEvent.click(model);
    expect(within(screen.getByRole('group', { name: 'settings.ai.models.recommended' })).getAllByRole('option').some((option) => option.value === 'glm-5.3-flash')).toBe(true);
    const other = within(screen.getByRole('group', { name: 'settings.ai.models.other' })).getAllByRole('option');
    fireEvent.click(other.find((option) => option.value === 'glm-5.3'));
    expect(model).toHaveValue('glm-5.3');
    selectOption('settings.ai.fields.model', 'custom');
    const custom = screen.getByRole('textbox', { name: 'settings.ai.fields.model_id' });
    expect(custom).toHaveValue('glm-5.3');
    fireEvent.change(custom, { target: { value: 'my-model' } });
    fireEvent.change(screen.getByLabelText('settings.ai.fields.base_url'), { target: { value: 'https://example.com/v1' } });
    fireEvent.change(screen.getByRole('combobox', { name: 'settings.ai.fields.context_mode' }), { target: { value: 'manual' } });
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
      provider: expect.objectContaining({ model: 'installed-model', context_tokens: null, api_key: null }),
    }));
    await screen.findByText('settings.ai.saved');
  });

  it('resets the model, key and budget when switching providers', async () => {
    render(<AiProvidersSection />);
    await addPreset('glm');
    fireEvent.change(screen.getByLabelText('settings.ai.fields.api_key'), { target: { value: 'glm-secret' } });
    selectOption('settings.ai.fields.preset', 'mimo');
    expect(screen.getByLabelText('settings.ai.fields.api_key')).toHaveValue('');
    expect(screen.getByLabelText('settings.ai.fields.model')).toHaveValue('mimo-v2.6-flash');
    expect(screen.getByRole('button', { name: 'common.save' })).toBeDisabled();
    selectOption('settings.ai.fields.preset', 'ollama');
    expect(screen.getByLabelText('settings.ai.fields.model')).toHaveValue('');
    expect(screen.getByLabelText('settings.ai.fields.context_tokens')).toHaveValue(32768);
    selectOption('settings.ai.fields.preset', 'custom');
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

describe('model discovery and image permission', () => {
  it('asks for cloud consent when a local preset uses a remote address', async () => {
    render(<AiProvidersSection />);
    await addPreset('ollama');
    fireEvent.change(screen.getByLabelText('settings.ai.fields.base_url'), { target: { value: 'https://example.com/v1' } });
    fireEvent.click(screen.getByText('settings.ai.advanced'));
    const permission = screen.getByRole('switch', { name: 'settings.ai.images.allow' });
    fireEvent.click(permission);
    expect(screen.getByRole('dialog', { name: 'settings.ai.images.consent_title' })).toBeInTheDocument();
    expect(permission).toHaveAttribute('aria-checked', 'false');
  });

  it('reads cached models without prompting, preserves the saved model and discovers on request', async () => {
    const fallback = invoke.getMockImplementation();
    invoke.mockImplementation(async (command, args) => {
      if (command === 'ai_discover_models') return {
        models: [{ id: args.refresh ? 'new-model' : 'cached-model', name: args.refresh ? 'New' : 'Cached', image_input: 'supported' }],
        stale: !args.refresh,
      };
      return fallback(command, args);
    });
    render(<AiProvidersSection />);
    await edit();
    await waitFor(() => expect(screen.getByRole('combobox', { name: 'settings.ai.fields.model' })).toHaveTextContent('settings.ai.models.custom'));
    expect(screen.getByLabelText('settings.ai.fields.model_id')).toHaveValue('local');
    expect(invoke).toHaveBeenCalledWith('ai_discover_models', {
      provider: { id: 'p', kind: 'openai_compatible', base_url: 'http://localhost/v1', api_key: null }, refresh: false,
    });
    expect(withAuth).toHaveBeenCalledWith(expect.any(Function), { autoPrompt: false });
    fireEvent.click(screen.getByRole('button', { name: 'settings.ai.models.discover' }));
    await screen.findByText('settings.ai.models.discovered');
    selectOption('settings.ai.fields.model', 'new-model');
    fireEvent.click(screen.getByRole('button', { name: 'common.save' }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('ai_save_provider', {
      provider: expect.objectContaining({ model: 'new-model', api_key: null }),
    }));
  });

  it('discards a late cache result after switching the endpoint', async () => {
    let finishCache;
    const fallback = invoke.getMockImplementation();
    invoke.mockImplementation((command, args) => command === 'ai_discover_models'
      ? new Promise((resolve) => { finishCache = resolve; }) : fallback(command, args));
    render(<AiProvidersSection />);
    await edit();
    fireEvent.change(screen.getByLabelText('settings.ai.fields.base_url'), { target: { value: 'http://localhost:9999/v1' } });
    await act(async () => finishCache({ models: [{ id: 'wrong-account-model' }], stale: false }));
    expect(screen.getByRole('textbox', { name: 'settings.ai.fields.model' })).toHaveValue('local');
    expect(screen.queryByRole('combobox', { name: 'settings.ai.fields.model' })).not.toBeInTheDocument();
  });

  it('requires cloud consent independently of image capability and revokes permission on endpoint edits', async () => {
    Object.assign(provider, { name: 'My Kimi', base_url: 'https://api.moonshot.cn/v1', model: 'kimi-k3' });
    render(<AiProvidersSection />);
    fireEvent.click(await screen.findByRole('button', { name: 'settings.ai.providers.edit My Kimi' }));
    fireEvent.click(screen.getByText('settings.ai.advanced'));
    const permission = screen.getByRole('switch', { name: 'settings.ai.images.allow' });
    expect(permission).toHaveAttribute('aria-checked', 'false');
    fireEvent.change(screen.getByRole('combobox', { name: 'settings.ai.images.capability' }), { target: { value: 'supported' } });
    expect(permission).toHaveAttribute('aria-checked', 'false');
    fireEvent.click(permission);
    const dialog = screen.getByRole('dialog', { name: 'settings.ai.images.consent_title' });
    expect(permission).toHaveAttribute('aria-checked', 'false');
    fireEvent.click(within(dialog).getByRole('button', { name: 'settings.ai.images.consent_accept' }));
    expect(permission).toHaveAttribute('aria-checked', 'true');
    fireEvent.click(screen.getByRole('button', { name: 'common.save' }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('ai_save_provider', {
      provider: expect.objectContaining({ allow_images: true, image_input_override: 'supported' }),
    }));
    await screen.findByText('settings.ai.saved');
    fireEvent.click(screen.getByRole('button', { name: 'settings.ai.providers.edit My Kimi' }));
    fireEvent.click(screen.getByText('settings.ai.advanced'));
    fireEvent.change(screen.getByLabelText('settings.ai.fields.base_url'), { target: { value: 'https://example.com/v1' } });
    expect(screen.getByRole('switch', { name: 'settings.ai.images.allow' })).toHaveAttribute('aria-checked', 'false');
    expect(screen.getByRole('combobox', { name: 'settings.ai.images.capability' })).toHaveValue('auto');
  });
});

async function edit({ manual = false } = {}) {
  fireEvent.click(await screen.findByRole('button', { name: 'settings.ai.providers.edit Local' }));
  fireEvent.click(screen.getByText('settings.ai.advanced'));
  if (manual) fireEvent.change(screen.getByRole('combobox', { name: 'settings.ai.fields.context_mode' }), { target: { value: 'manual' } });
  return screen.getByRole('spinbutton', { name: 'settings.ai.fields.context_tokens' });
}

describe('AI context budget settings', () => {
  it('adapts new services to model capacity and keeps an explicit limit when switching models', async () => {
    const fallback = invoke.getMockImplementation();
    invoke.mockImplementation(async (command, args) => {
      if (command === 'ai_get_model_catalog') return { providers: [{
        id: 'catalog', name: 'Catalog', kind: 'openai_compatible', baseUrl: 'https://api.example.com/v1',
        models: [
          { id: 'large', name: 'Large', recommended: true, context_tokens: 1048576 },
          { id: 'small', name: 'Small', context_tokens: 64000 },
        ],
      }] };
      return fallback(command, args);
    });
    render(<AiProvidersSection />);
    await addPreset('catalog');
    fireEvent.click(screen.getByText('settings.ai.advanced'));
    const input = screen.getByLabelText('settings.ai.fields.context_tokens');
    expect(input).toBeDisabled();
    expect(input).toHaveValue(1048576);
    selectOption('settings.ai.fields.model', 'small');
    expect(input).toHaveValue(64000);
    const mode = screen.getByRole('combobox', { name: 'settings.ai.fields.context_mode' });
    fireEvent.change(mode, { target: { value: 'manual' } });
    expect(input).toBeEnabled();
    fireEvent.change(input, { target: { value: '48000' } });
    selectOption('settings.ai.fields.model', 'large');
    expect(input).toHaveValue(48000);
    fireEvent.change(mode, { target: { value: 'auto' } });
    expect(input).toHaveValue(1048576);
    fireEvent.change(screen.getByLabelText('settings.ai.fields.api_key'), { target: { value: 'test-key' } });
    fireEvent.click(screen.getByRole('button', { name: 'common.save' }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('ai_save_provider', {
      provider: expect.objectContaining({ model: 'large', context_tokens: null }),
    }));
  });

  it('preserves a legacy numeric limit and lets the user switch to fresh discovered model capacity', async () => {
    provider.context_tokens = 32768;
    const fallback = invoke.getMockImplementation();
    invoke.mockImplementation(async (command, args) => command === 'ai_discover_models'
      ? { models: [{ id: 'local', name: 'Local', context_tokens: 128000 }], stale: false }
      : fallback(command, args));
    render(<AiProvidersSection />);
    const input = await edit();
    await waitFor(() => expect(screen.getByRole('combobox', { name: 'settings.ai.fields.model' })).toBeInTheDocument());
    expect(input).toBeEnabled();
    expect(input).toHaveValue(32768);
    fireEvent.change(screen.getByRole('combobox', { name: 'settings.ai.fields.context_mode' }), { target: { value: 'auto' } });
    expect(input).toBeDisabled();
    expect(input).toHaveValue(128000);
    fireEvent.click(screen.getByRole('button', { name: 'common.save' }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('ai_save_provider', {
      provider: expect.objectContaining({ id: 'p', context_tokens: null, api_key: null }),
    }));
  });

  it('defaults old providers, validates edits and saves through authenticated settings', async () => {
    const view = render(<AiProvidersSection />);
    const input = await edit({ manual: true });
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
