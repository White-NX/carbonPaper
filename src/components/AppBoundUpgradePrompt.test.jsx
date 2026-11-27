import React from 'react';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import AppBoundUpgradePrompt from './AppBoundUpgradePrompt';
import { showDebugOverlay } from './overlay';
import { OverlayCoordinatorProvider } from './overlay/coordinator';
import { DialogVisibilityContext } from '../hooks/useDialogFocus';
import { dismissAppBoundOffer, getAppBoundStatus, installAppBound, repairAppBound } from '../lib/app_bound_api';
import { selectAppBoundRepairPackage } from '../lib/native_dialogs';

vi.mock('react-i18next', async () => {
  const { createInstance } = await import('i18next');
  const { default: translation } = await import('../i18n/locales/zh-CN.json');
  const i18n = createInstance();
  await i18n.init({ lng: 'zh-CN', resources: { 'zh-CN': { translation } } });
  const t = i18n.getFixedT('zh-CN');
  return { useTranslation: () => ({ t }) };
});
vi.mock('../lib/app_bound_api', async (importOriginal) => ({
  ...await importOriginal(), getAppBoundStatus: vi.fn(), installAppBound: vi.fn(), repairAppBound: vi.fn(), dismissAppBoundOffer: vi.fn(),
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('../lib/native_dialogs', () => ({ selectAppBoundRepairPackage: vi.fn() }));

beforeEach(() => {
  vi.resetAllMocks();
  getAppBoundStatus.mockResolvedValue({ offer_enable: true });
  dismissAppBoundOffer.mockResolvedValue(undefined);
  repairAppBound.mockResolvedValue({ status: 'repaired', restart_required: false });
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

  it('repair uses the independent repair flow and preserves the existing enabled preference', async () => {
    getAppBoundStatus.mockResolvedValue({ offer_enable: true, reason: 'repair_required' });
    render(<AppBoundUpgradePrompt visible />);
    fireEvent.click(await screen.findByRole('button', { name: '修复组件' }));
    await waitFor(() => expect(repairAppBound).toHaveBeenCalledWith({}));
    expect(installAppBound).not.toHaveBeenCalled();
  });

  it('cancelling repair elevation keeps the repair offer available', async () => {
    getAppBoundStatus.mockResolvedValue({ offer_enable: true, reason: 'repair_required' });
    repairAppBound.mockRejectedValue('APP_BOUND_CANCELLED');
    render(<AppBoundUpgradePrompt visible />);
    fireEvent.click(await screen.findByRole('button', { name: '修复组件' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('已取消，当前设置保持不变。');
    expect(screen.getByRole('button', { name: '修复组件' })).toBeEnabled();
    expect(dismissAppBoundOffer).not.toHaveBeenCalled();
  });

  it('cancelling local package selection does not start repair', async () => {
    getAppBoundStatus.mockResolvedValue({ offer_enable: true, reason: 'repair_required' });
    selectAppBoundRepairPackage.mockResolvedValue(null);
    render(<AppBoundUpgradePrompt visible />);
    fireEvent.click(await screen.findByRole('button', { name: '从本地包修复' }));
    await waitFor(() => expect(selectAppBoundRepairPackage).toHaveBeenCalledTimes(1));
    expect(repairAppBound).not.toHaveBeenCalled();
  });

  it('a file selection completed after the view closes does not start repair', async () => {
    getAppBoundStatus.mockResolvedValue({ offer_enable: true, reason: 'repair_required' });
    let selected;
    selectAppBoundRepairPackage.mockImplementation(() => new Promise(resolve => { selected = resolve; }));
    const view = render(<AppBoundUpgradePrompt visible />);
    fireEvent.click(await screen.findByRole('button', { name: '从本地包修复' }));
    view.unmount();
    await act(async () => selected('D:/Downloads/repair.zip'));
    expect(repairAppBound).not.toHaveBeenCalled();
  });

  it('requires a separate decision before downloading the complete runtime', async () => {
    getAppBoundStatus.mockResolvedValue({ offer_enable: true, reason: 'repair_required' });
    repairAppBound.mockResolvedValueOnce({ status: 'download_required', size_bytes: 200 * 1024 * 1024 });
    render(<OverlayCoordinatorProvider value={{ active: 'appBound' }}>
      <AppBoundUpgradePrompt visible />
    </OverlayCoordinatorProvider>);
    fireEvent.click(await screen.findByRole('button', { name: '修复组件' }));
    const download = await screen.findByRole('button', { name: '下载并修复' });
    expect(screen.getByText(/预计约 200 MB/)).toBeInTheDocument();
    expect(repairAppBound).toHaveBeenCalledTimes(1);
    fireEvent.click(download);
    await waitFor(() => expect(repairAppBound).toHaveBeenNthCalledWith(2, { allowFullDownload: true }));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
  });

  it('declining the complete download keeps the repair offer available', async () => {
    getAppBoundStatus.mockResolvedValue({ offer_enable: true, reason: 'repair_required' });
    repairAppBound.mockResolvedValue({ status: 'download_required', size_bytes: null });
    render(<AppBoundUpgradePrompt visible />);
    fireEvent.click(await screen.findByRole('button', { name: '修复组件' }));
    const dialog = await screen.findByRole('dialog', { name: '下载完整程序包' });
    fireEvent.click(within(dialog).getByRole('button', { name: '稍后' }));
    expect(repairAppBound).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('button', { name: '修复组件' })).toBeEnabled();
  });

  it('suspends a pending download decision for authentication and other startup overlays', async () => {
    getAppBoundStatus.mockResolvedValue({ offer_enable: true, reason: 'repair_required' });
    repairAppBound.mockResolvedValue({ status: 'download_required', size_bytes: null });
    const promptTree = (active, authenticated = true) => (
      <DialogVisibilityContext.Provider value={authenticated}>
        <OverlayCoordinatorProvider value={{ active }}>
          <AppBoundUpgradePrompt visible />
        </OverlayCoordinatorProvider>
      </DialogVisibilityContext.Provider>
    );
    const view = render(promptTree('appBound'));
    fireEvent.click(await screen.findByRole('button', { name: '修复组件' }));
    expect(await screen.findByRole('dialog', { name: '下载完整程序包' })).toBeInTheDocument();
    for (const [active, authenticated] of [['appBound', false], ['auth', false], ['maintenance', true]]) {
      view.rerender(promptTree(active, authenticated));
      expect(screen.queryByRole('dialog', { name: '下载完整程序包' })).not.toBeInTheDocument();
      expect(repairAppBound).toHaveBeenCalledTimes(1);
    }
    view.rerender(promptTree('appBound'));
    const dialog = await screen.findByRole('dialog', { name: '下载完整程序包' });
    fireEvent.click(within(dialog).getByRole('button', { name: '稍后' }));
    expect(screen.getByRole('button', { name: '修复组件' })).toBeEnabled();
    expect(repairAppBound).toHaveBeenCalledTimes(1);
  });

  it('offers a manual package upgrade without activating it on startup or enabling a disabled policy', async () => {
    getAppBoundStatus.mockResolvedValue({ offer_enable: true, reason: 'upgrade_required' });
    installAppBound.mockRejectedValue('APP_BOUND_CANCELLED');
    render(<AppBoundUpgradePrompt visible />);
    const upgrade = await screen.findByRole('button', { name: '更新后台组件' });
    expect(installAppBound).not.toHaveBeenCalled();
    fireEvent.click(upgrade);
    expect(await screen.findByRole('alert')).toHaveTextContent('已取消');
    expect(installAppBound).toHaveBeenCalledWith(false);
    expect(upgrade).toBeEnabled();
  });

  it('does not reopen an acknowledged offer', async () => {
    getAppBoundStatus.mockResolvedValue({ offer_enable: false });
    render(<AppBoundUpgradePrompt visible />);
    await waitFor(() => expect(getAppBoundStatus).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('debug preview forces the dialog over a closed UI without backend checks', async () => {
    render(<AppBoundUpgradePrompt visible={false} />);
    act(() => showDebugOverlay('appBound'));
    expect(await screen.findByRole('dialog')).toHaveTextContent('重启后继续后台整理');
    expect(screen.getByRole('button', { name: '启用' })).toBeInTheDocument();
    expect(getAppBoundStatus).not.toHaveBeenCalled();
  });

  it('debug preview can show the repair variant', async () => {
    render(<AppBoundUpgradePrompt visible={false} />);
    act(() => showDebugOverlay('appBound', 'repair'));
    expect(await screen.findByRole('button', { name: '修复组件' })).toBeInTheDocument();
  });

  it('debug preview closes without acknowledging the real offer', async () => {
    render(<AppBoundUpgradePrompt visible={false} />);
    act(() => showDebugOverlay('appBound'));
    fireEvent.click(await screen.findByRole('button', { name: '稍后' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(dismissAppBoundOffer).not.toHaveBeenCalled();
  });
});
