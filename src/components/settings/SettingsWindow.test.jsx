import React, { useState } from 'react';
import { act, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { useSettingsActivity } from './SettingsActivityContext';
import { ConfirmDialog } from '../ConfirmDialog';
import SettingsWindow from './SettingsWindow';

const mocks = vi.hoisted(() => ({ listeners: new Map(), auth: { isAuthenticated: true }, minimize: vi.fn(), setTitle: vi.fn(async () => {}), toggleMaximize: vi.fn() }));
vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => mocks }));
vi.mock('../../hooks/useTauriEventListener', () => ({ useTauriEventListener: (name, handler) => mocks.listeners.set(name, handler) }));
vi.mock('../../hooks/useAppTheme', () => ({ useAppTheme: () => ({}) }));
vi.mock('../../hooks/useAuthSession', () => ({ useAuthSession: () => mocks.auth }));
vi.mock('../../hooks/useAppWindowState', () => ({ usePowerSavingState: () => ({ powerSavingMode: true }) }));
vi.mock('../../lib/auth_api', () => ({ initAuthListeners: () => () => {}, withAuth: (call) => call() }));
vi.mock('../AuthMask', () => ({ default: () => <div>Locked settings</div> }));
vi.mock('./SettingsContent', () => ({ default: function Content() {
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  const [modal, setModal] = useState(false);
  useSettingsActivity('editor', { dirty, busy });
  return <div><span>Private settings</span><button onClick={() => setDirty(true)}>Edit</button><button onClick={() => setBusy(true)}>Start migration</button>
    <button onClick={() => setModal(true)}>Private dialog</button><ConfirmDialog isOpen={modal} title="Private record name" onCancel={() => setModal(false)} /></div>;
} }));

beforeEach(() => {
  mocks.auth = { isAuthenticated: true };
  mocks.listeners.clear();
  invoke.mockReset().mockResolvedValue(null);
});

describe('settings window lifecycle', () => {
  it('closes a clean window through the native boundary', async () => {
    render(<SettingsWindow />);
    await userEvent.click(screen.getByRole('button', { name: 'common.close' }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('close_settings_window', { lightweight: false }));
  });
  it('retains drafts until the user explicitly discards them', async () => {
    render(<SettingsWindow />);
    await userEvent.click(screen.getByText('Edit'));
    await userEvent.click(screen.getByRole('button', { name: 'common.close' }));
    expect(screen.getByRole('dialog')).toHaveTextContent('settings.window.unsavedTitle');
    expect(invoke.mock.calls.some(([command]) => command === 'close_settings_window')).toBe(false);
    await userEvent.click(screen.getByText('settings.window.keepEditing'));
    await userEvent.click(screen.getByRole('button', { name: 'common.close' }));
    await userEvent.click(screen.getByText('settings.window.discardAndClose'));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('close_settings_window', { lightweight: false }));
  });
  it('keeps migration alive for native close and lightweight requests while permitting minimize', async () => {
    render(<SettingsWindow />);
    await userEvent.click(screen.getByText('Start migration'));
    act(() => mocks.listeners.get('settings-close-requested')({ payload: true }));
    expect(screen.getByRole('alert')).toHaveTextContent('settings.window.operationInProgress');
    expect(invoke.mock.calls.some(([command]) => command === 'close_settings_window')).toBe(false);
    await userEvent.click(screen.getByLabelText('settings.window.minimize'));
    expect(mocks.minimize).toHaveBeenCalled();
  });
  it('hides both settings and portalled dialogs when the shared session locks', async () => {
    const view = render(<SettingsWindow />);
    await userEvent.click(screen.getByText('Private dialog'));
    expect(screen.getByRole('dialog')).toHaveTextContent('Private record name');
    mocks.auth = { isAuthenticated: false };
    view.rerender(<SettingsWindow />);
    expect(screen.getByText('Private settings')).not.toBeVisible();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(screen.getByText('Locked settings')).toBeVisible();
  });
});
