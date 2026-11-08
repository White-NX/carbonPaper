import React, { useState } from 'react';
import { act, fireEvent, render, renderHook, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { acknowledgeSecurityIntro } from '../../../lib/agent_access_api';
import { OverlayCoordinatorProvider } from '../../overlay/coordinator';
import { useSecurityWizard } from './useSecurityWizard';
import SecurityWizard from './SecurityWizard';

vi.mock('react-i18next', async () => {
  const { createInstance } = await import('i18next');
  const { default: translation } = await import('../../../i18n/locales/zh-CN.json');
  const i18n = createInstance();
  await i18n.init({ lng: 'zh-CN', resources: { 'zh-CN': { translation } } });
  const t = i18n.getFixedT('zh-CN');
  return { useTranslation: () => ({ t, i18n }) };
});
vi.mock('../../../lib/agent_access_api', () => ({ acknowledgeSecurityIntro: vi.fn(async () => {}) }));
const policy = { level: 'strict', session_timeout: 900, unattended_enabled: false, available: true };
let security;
let closed;
beforeEach(() => {
  security = { policy, saving: false, error: '', apply: vi.fn(async () => true) };
  closed = vi.fn();
});
function Harness({ presentation = 'dialog' }) {
  const [open, setOpen] = useState(true);
  const wizard = useSecurityWizard({ open, security, onClose: () => { setOpen(false); closed(); } });
  return <SecurityWizard open={open} wizard={wizard} presentation={presentation} />;
}
async function chooseSettings() {
  await screen.findByRole('button', { name: '下一步' });
  await userEvent.click(screen.getByRole('button', { name: '下一步' }));
  expect(screen.getByRole('dialog')).toHaveTextContent('凭据泄露');
  await userEvent.click(screen.getByRole('button', { name: '下一步' }));
}

describe('security feature wizard', () => {
  it('keeps detailed edits in a draft and leaving preserves the current settings', async () => {
    render(<Harness />);
    await chooseSettings();
    await userEvent.click(screen.getByRole('radio', { name: /^自定义 / }));
    fireEvent.change(screen.getByRole('combobox', { name: '自动锁定' }), { target: { value: '3600' } });
    expect(security.apply).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole('button', { name: '保留当前设置' }));
    await waitFor(() => expect(closed).toHaveBeenCalledTimes(1));
    expect(acknowledgeSecurityIntro).toHaveBeenCalledTimes(1);
    expect(security.apply).not.toHaveBeenCalled();
  });

  it('handles the unattended risk decision inside an active startup overlay, then applies only on Finish', async () => {
    render(<OverlayCoordinatorProvider value={{ active: 'securityOnboarding', request: () => {} }}><Harness presentation="startup" /></OverlayCoordinatorProvider>);
    await chooseSettings();
    await userEvent.click(screen.getByRole('radio', { name: /^无人值守 / }));
    expect(screen.getByRole('dialog', { name: '开启无人值守访问？' })).toHaveTextContent('降低 CarbonPaper');
    expect(security.apply).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole('button', { name: '取消' }));
    expect(screen.getByRole('radio', { name: /^严格 / })).toHaveAttribute('aria-checked', 'true');
    await userEvent.click(screen.getByRole('radio', { name: /^无人值守 / }));
    await userEvent.click(screen.getByRole('button', { name: '接受风险并开启' }));
    expect(security.apply).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole('button', { name: '应用设置' }));
    await waitFor(() => expect(security.apply).toHaveBeenCalledWith(expect.objectContaining({ level: 'unattended', session_timeout: -1, unattended_enabled: true })));
    await waitFor(() => expect(closed).toHaveBeenCalledTimes(1));
  });

  it('warns for a custom switch and saves its refined timeout without substituting a preset', async () => {
    render(<Harness />);
    await chooseSettings();
    await userEvent.click(screen.getByRole('radio', { name: /^自定义 / }));
    fireEvent.change(screen.getByRole('combobox', { name: '自动锁定' }), { target: { value: '300' } });
    await userEvent.click(screen.getByRole('switch', { name: '无人值守智能体访问' }));
    await userEvent.click(screen.getByRole('button', { name: '接受风险并开启' }));
    await userEvent.click(screen.getByRole('button', { name: '应用设置' }));
    await waitFor(() => expect(security.apply).toHaveBeenCalledWith(expect.objectContaining({ level: 'custom', session_timeout: 300, unattended_enabled: true })));
  });

  it('refreshes component availability while preserving unsaved choices', async () => {
    const { result, rerender } = renderHook(({ current }) => useSecurityWizard({ open: true, security: current, onClose: closed }), { initialProps: { current: security } });
    act(() => result.current.change({ ...policy, level: 'custom', session_timeout: 3600 }));
    rerender({ current: { ...security, policy: { ...policy, available: false } } });
    await waitFor(() => expect(result.current.draft).toMatchObject({ level: 'custom', session_timeout: 3600, available: false }));
    expect(security.apply).not.toHaveBeenCalled();
  });

  it('keeps unattended Apply unavailable until the required components are available', async () => {
    security = { ...security, policy: { ...policy, available: false } };
    render(<Harness />);
    await chooseSettings();
    await userEvent.click(screen.getByRole('radio', { name: /^无人值守 / }));
    await userEvent.click(screen.getByRole('button', { name: '接受风险并开启' }));
    expect(screen.getByRole('button', { name: '应用设置' })).toBeDisabled();
    expect(screen.getByRole('dialog')).toHaveTextContent('安装或修复组件');
    expect(security.apply).not.toHaveBeenCalled();
  });
});
