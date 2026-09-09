import React from 'react';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import ProtectedProcessingCard from './ProtectedProcessingCard';
import { getAppBoundStatus, installAppBound, setAppBoundPolicy, uninstallAppBound } from '../../../lib/app_bound_api';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (_key, fallback) => fallback }) }));
vi.mock('../../../lib/app_bound_api', async (importOriginal) => ({
  ...await importOriginal(), getAppBoundStatus: vi.fn(), installAppBound: vi.fn(), setAppBoundPolicy: vi.fn(), uninstallAppBound: vi.fn(),
}));

const active = { installed: true, supported: true, package_available: true, enabled: true, available: true,
  limits: { retention_days: 30, capacity_bytes: 4096 * 1024 * 1024 } };

beforeEach(() => {
  vi.resetAllMocks();
  getAppBoundStatus.mockResolvedValue(active);
  setAppBoundPolicy.mockResolvedValue(undefined);
  installAppBound.mockResolvedValue(undefined);
  uninstallAppBound.mockResolvedValue(undefined);
});

describe('ProtectedProcessingCard', () => {
  it('validates both limits before saving', async () => {
    render(<ProtectedProcessingCard />);
    const days = await screen.findByRole('spinbutton', { name: '待处理内容保留天数' });
    const capacity = screen.getByRole('spinbutton', { name: '暂存容量上限（MiB）' });
    const save = screen.getByRole('button', { name: '保存限制' });
    fireEvent.change(days, { target: { value: '0' } });
    expect(save).toBeDisabled();
    fireEvent.change(days, { target: { value: '1' } });
    fireEvent.change(capacity, { target: { value: '255' } });
    expect(save).toBeDisabled();
    fireEvent.change(capacity, { target: { value: '256' } });
    fireEvent.click(save);
    await waitFor(() => expect(setAppBoundPolicy).toHaveBeenCalledWith(true, 1, 256));
  });

  it('can disable processing even while limits contain an unfinished edit', async () => {
    render(<ProtectedProcessingCard />);
    const days = await screen.findByRole('spinbutton', { name: '待处理内容保留天数' });
    fireEvent.change(days, { target: { value: '' } });
    fireEvent.click(screen.getByRole('button', { name: '关闭' }));
    await waitFor(() => expect(setAppBoundPolicy).toHaveBeenCalledWith(false, 30, 4096));
  });

  it('repairs a disabled component while the background master switch is off', async () => {
    getAppBoundStatus.mockResolvedValue({ ...active, enabled: false, available: false, reason: 'repair_required' });
    render(<ProtectedProcessingCard backgroundEnabled={false} />);
    const repair = await screen.findByRole('button', { name: '修复组件' });
    expect(repair).toBeEnabled();
    fireEvent.click(repair);
    await waitFor(() => expect(installAppBound).toHaveBeenCalledWith(false));
    expect(setAppBoundPolicy).not.toHaveBeenCalled();
  });

  it('does not enable processing while the background master switch is off', async () => {
    getAppBoundStatus.mockResolvedValue({ ...active, installed: false, enabled: false, available: false });
    render(<ProtectedProcessingCard backgroundEnabled={false} />);
    await waitFor(() => expect(getAppBoundStatus).toHaveBeenCalled());
    expect(screen.getByRole('button', { name: '启用' })).toBeDisabled();
  });

  it('keeps current status after cancelled removal', async () => {
    uninstallAppBound.mockRejectedValue('APP_BOUND_CANCELLED');
    render(<ProtectedProcessingCard />);
    fireEvent.click(await screen.findByRole('button', { name: '移除后台组件' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('已取消');
    expect(screen.getByRole('status')).toHaveTextContent('已开启');
  });
});
