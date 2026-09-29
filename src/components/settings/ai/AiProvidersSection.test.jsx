import React from 'react';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
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
    if (command === 'ai_save_provider') provider = { ...provider, ...args.provider };
    return { providers: [provider], default_provider_id: 'p' };
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
