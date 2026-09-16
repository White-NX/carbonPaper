import React from 'react';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import BrowserExtensionSection from './BrowserExtensionSection';

const t = (key) => key;
vi.mock('react-i18next', () => ({ useTranslation: () => ({ t }) }));
vi.mock('../../lib/auth_api', () => ({ withAuth: (call) => call() }));
let host;
beforeEach(() => {
  host = { chrome: true, edge: false, extension_path: null };
  invoke.mockReset().mockImplementation(async (command, args) => {
    if (command === 'get_nm_host_status') return { ...host };
    if (command === 'get_extension_enhancement_config') return { enabled: true };
    if (command === 'get_nmh_sessions') return [];
    if (command === 'install_browser_extension') { host[args.browser] = true; host.extension_path = 'C:\\CarbonPaper\\extension'; return { extension_path: host.extension_path }; }
    return null;
  });
});

describe('browser extension setup', () => {
  it('distinguishes native registration from an actual browser connection', async () => {
    render(<BrowserExtensionSection />);
    expect(await screen.findByText('settings.extension.status.configured')).toBeVisible();
    expect(screen.getByText('settings.extension.status.not_configured')).toBeVisible();
    expect(screen.queryByText('settings.extension.status.connected')).not.toBeInTheDocument();
  });
  it('reveals installation instructions and the returned folder after setup', async () => {
    const user = userEvent.setup();
    render(<BrowserExtensionSection />);
    await screen.findByText('settings.extension.status.configured');
    await user.click(screen.getByText('settings.extension.setup'));
    expect(await screen.findByText('C:\\CarbonPaper\\extension')).toBeVisible();
    expect(screen.getByText('settings.extension.tutorial.step3')).toBeVisible();
    expect(invoke).toHaveBeenCalledWith('install_browser_extension', { browser: 'edge' });
  });
  it('reports a failed read without claiming that the browser is unconfigured', async () => {
    invoke.mockRejectedValue(new Error('offline'));
    render(<BrowserExtensionSection />);
    expect(await screen.findByRole('alert')).toHaveTextContent('settings.extension.readFailed');
    expect(screen.getAllByText('settings.extension.status.unavailable')).toHaveLength(2);
    expect(screen.queryByText('settings.extension.status.not_configured')).not.toBeInTheDocument();
  });
  it('retains the enhancement preference when saving fails', async () => {
    const original = invoke.getMockImplementation();
    invoke.mockImplementation((command, args) => command === 'set_extension_enhancement' ? Promise.reject(new Error('denied')) : original(command, args));
    render(<BrowserExtensionSection />);
    const toggle = screen.getByRole('switch');
    await waitFor(() => expect(toggle).toBeEnabled());
    fireEvent.click(toggle);
    expect(await screen.findByRole('alert')).toHaveTextContent('settings.feedback.saveFailed');
    expect(toggle).toHaveAttribute('aria-checked', 'true');
  });
});
