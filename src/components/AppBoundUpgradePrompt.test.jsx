import React from 'react';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import AppBoundUpgradePrompt from './AppBoundUpgradePrompt';
import { dismissAppBoundOffer, getAppBoundStatus, installAppBound } from '../lib/app_bound_api';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (_key, fallback) => fallback }) }));
vi.mock('../lib/app_bound_api', async (importOriginal) => ({
  ...await importOriginal(), getAppBoundStatus: vi.fn(), installAppBound: vi.fn(), dismissAppBoundOffer: vi.fn(),
}));

beforeEach(() => {
  vi.resetAllMocks();
  getAppBoundStatus.mockResolvedValue({ offer_enable: true });
  dismissAppBoundOffer.mockResolvedValue(undefined);
});

describe('AppBoundUpgradePrompt', () => {
  it('waits for an unlocked visible UI and only presents an eligible offer', async () => {
    const view = render(<AppBoundUpgradePrompt visible={false} />);
    expect(getAppBoundStatus).not.toHaveBeenCalled();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    view.rerender(<AppBoundUpgradePrompt visible />);
    expect(await screen.findByRole('dialog')).toHaveTextContent('重启后继续后台整理');
    expect(installAppBound).not.toHaveBeenCalled();
  });

  it('Later acknowledges the offer without starting activation', async () => {
    render(<AppBoundUpgradePrompt visible />);
    fireEvent.click(await screen.findByRole('button', { name: '稍后' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(dismissAppBoundOffer).toHaveBeenCalledTimes(1);
    expect(installAppBound).not.toHaveBeenCalled();
  });

  it('UAC cancellation leaves the offer usable and reports that settings did not change', async () => {
    installAppBound.mockRejectedValue('APP_BOUND_CANCELLED');
    render(<AppBoundUpgradePrompt visible />);
    fireEvent.click(await screen.findByRole('button', { name: '启用' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('已取消，当前设置保持不变。');
    expect(installAppBound).toHaveBeenCalledWith(true);
    expect(screen.getByRole('button', { name: '启用' })).toBeEnabled();
    expect(dismissAppBoundOffer).not.toHaveBeenCalled();
  });

  it('repair preserves the existing enabled preference', async () => {
    getAppBoundStatus.mockResolvedValue({ offer_enable: true, reason: 'repair_required' });
    installAppBound.mockResolvedValue(undefined);
    render(<AppBoundUpgradePrompt visible />);
    fireEvent.click(await screen.findByRole('button', { name: '修复组件' }));
    await waitFor(() => expect(installAppBound).toHaveBeenCalledWith(false));
  });

  it('does not reopen an acknowledged offer', async () => {
    getAppBoundStatus.mockResolvedValue({ offer_enable: false });
    render(<AppBoundUpgradePrompt visible />);
    await waitFor(() => expect(getAppBoundStatus).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('debug preview forces the dialog over a closed UI without backend checks', async () => {
    render(<AppBoundUpgradePrompt visible={false} />);
    window.dispatchEvent(new CustomEvent('debug-show-app-bound-offer'));
    expect(await screen.findByRole('dialog')).toHaveTextContent('重启后继续后台整理');
    expect(screen.getByRole('button', { name: '启用' })).toBeInTheDocument();
    expect(getAppBoundStatus).not.toHaveBeenCalled();
  });

  it('debug preview can show the repair variant', async () => {
    render(<AppBoundUpgradePrompt visible={false} />);
    window.dispatchEvent(new CustomEvent('debug-show-app-bound-offer', { detail: { repair: true } }));
    expect(await screen.findByRole('button', { name: '修复组件' })).toBeInTheDocument();
  });

  it('debug preview closes without acknowledging the real offer', async () => {
    render(<AppBoundUpgradePrompt visible={false} />);
    window.dispatchEvent(new CustomEvent('debug-show-app-bound-offer'));
    fireEvent.click(await screen.findByRole('button', { name: '稍后' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(dismissAppBoundOffer).not.toHaveBeenCalled();
  });
});
