import React from 'react';
import { act, fireEvent, render, renderHook, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../lib/auth_api';
import SecuritySection from './SecuritySection';
import { useSecurityLevel } from './security/useSecurityLevel';

const { events } = vi.hoisted(() => ({ events: new Map() }));
vi.mock('react-i18next', async () => {
  const { createInstance } = await import('i18next');
  const { default: translation } = await import('../../i18n/locales/zh-CN.json');
  const i18n = createInstance();
  await i18n.init({ lng: 'zh-CN', resources: { 'zh-CN': { translation } } });
  const t = i18n.getFixedT('zh-CN');
  return { useTranslation: () => ({ t, i18n }) };
});
vi.mock('../../lib/auth_api', () => ({ withAuth: vi.fn((operation) => operation()) }));
vi.mock('../../lib/settings_api', () => ({ notifySettingsChanged: vi.fn(async () => {}), openSettingsWindow: vi.fn(async () => {}) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async (name, handler) => {
  events.set(name, handler);
  return () => { if (events.get(name) === handler) events.delete(name); };
}) }));

const initial = { level: 'strict', session_timeout: 900, unattended_enabled: false, available: true, phase: 'ready', preparation: {}, intro_seen: true };
let policy;
let saveError;
beforeEach(() => {
  events.clear(); localStorage.clear(); policy = { ...initial }; saveError = null;
  invoke.mockReset().mockImplementation(async (command, args) => {
    if (command === 'security_get_policy') return policy;
    if (command === 'security_set_policy') {
      if (saveError) throw saveError;
      policy = { ...policy, level: args.level, session_timeout: args.sessionTimeout, unattended_enabled: args.unattendedEnabled };
      return policy;
    }
  });
});

const saves = () => invoke.mock.calls.filter(([command]) => command === 'security_set_policy');
const unattended = () => screen.getByRole('radio', { name: /^无人值守 / });
const custom = () => screen.getByRole('radio', { name: /^自定义 / });

describe('security level settings', () => {
  it('applies presets together and preserves explicit Custom and its refinements', async () => {
    const onTimeout = vi.fn();
    render(<SecuritySection isSessionValid onSessionTimeoutChange={onTimeout} />);
    await screen.findByRole('radio', { name: /^严格 / });
    expect(screen.queryByRole('combobox')).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole('radio', { name: /^宽松（建议）/ }));
    await waitFor(() => expect(saves().at(-1)?.[1]).toEqual({ level: 'relaxed', sessionTimeout: -1, unattendedEnabled: false }));
    await waitFor(() => expect(onTimeout).toHaveBeenCalledWith(-1));
    await userEvent.click(custom());
    await waitFor(() => expect(custom()).toHaveAttribute('aria-checked', 'true'));
    expect(screen.getByRole('combobox', { name: '自动锁定' })).toHaveValue('-1');
    fireEvent.change(screen.getByRole('combobox', { name: '自动锁定' }), { target: { value: '3600' } });
    await waitFor(() => expect(saves().at(-1)?.[1]).toEqual({ level: 'custom', sessionTimeout: 3600, unattendedEnabled: false }));
    expect(withAuth).toHaveBeenLastCalledWith(expect.any(Function), { promptMode: 'action' });
  });

  it('requires a risk decision before enabling the Unattended preset and cancellation keeps Strict', async () => {
    render(<SecuritySection isSessionValid />);
    await screen.findByRole('radio', { name: /^严格 / });
    await userEvent.click(unattended());
    expect(screen.getByRole('dialog')).toHaveTextContent('降低 CarbonPaper 的安全性');
    expect(screen.getByRole('dialog')).toHaveTextContent('宽松（建议）');
    expect(saves()).toHaveLength(0);
    await userEvent.click(screen.getByRole('button', { name: '取消' }));
    expect(screen.getByRole('radio', { name: /^严格 / })).toHaveAttribute('aria-checked', 'true');
    expect(saves()).toHaveLength(0);
    await userEvent.click(unattended());
    await userEvent.click(screen.getByRole('button', { name: '接受风险并开启' }));
    await waitFor(() => expect(saves().at(-1)?.[1]).toEqual({ level: 'unattended', sessionTimeout: -1, unattendedEnabled: true }));
    await waitFor(() => expect(unattended()).toHaveAttribute('aria-checked', 'true'));
  });

  it('also warns for the custom permission switch while retaining its chosen timeout', async () => {
    policy = { ...policy, level: 'custom', session_timeout: 300 };
    render(<SecuritySection isSessionValid />);
    const toggle = await screen.findByRole('switch', { name: '无人值守智能体访问' });
    await userEvent.click(toggle);
    expect(saves()).toHaveLength(0);
    await userEvent.click(screen.getByRole('button', { name: '取消' }));
    expect(toggle).toHaveAttribute('aria-checked', 'false');
    await userEvent.click(toggle);
    await userEvent.click(screen.getByRole('button', { name: '接受风险并开启' }));
    await waitFor(() => expect(saves().at(-1)?.[1]).toEqual({ level: 'custom', sessionTimeout: 300, unattendedEnabled: true }));
    await waitFor(() => expect(toggle).toHaveAttribute('aria-checked', 'true'));
    await userEvent.click(unattended());
    expect(screen.getByRole('dialog')).toHaveTextContent('宽松（建议）');
    expect(saves()).toHaveLength(1);
  });

  it.each(['AUTH_CANCELLED', 'Verification failed: User cancelled authentication'])('keeps the effective policy after cancelled verification: %s', async (error) => {
    saveError = new Error(error);
    render(<SecuritySection isSessionValid />);
    await screen.findByRole('radio', { name: /^严格 / });
    await userEvent.click(unattended());
    await userEvent.click(screen.getByRole('button', { name: '接受风险并开启' }));
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('已取消，保留当前设置'));
    expect(screen.getByRole('radio', { name: /^严格 / })).toHaveAttribute('aria-checked', 'true');
    act(() => events.get('agent-access-changed')({ payload: {} }));
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('已取消'));
  });

  it('does not display a guessed preset when the initial policy read fails', async () => {
    invoke.mockRejectedValue(new Error('unavailable'));
    render(<SecuritySection isSessionValid />);
    await screen.findByRole('alert');
    expect(screen.queryByRole('radio')).not.toBeInTheDocument();
    invoke.mockResolvedValue(policy);
    await userEvent.click(screen.getByRole('button', { name: '重试' }));
    expect(await screen.findByRole('radio', { name: /^严格 / })).toHaveAttribute('aria-checked', 'true');
  });

  it('queues a fresh cross-window policy read when a change arrives during a slow read', async () => {
    let resolveRead;
    invoke.mockImplementationOnce(() => new Promise((resolve) => { resolveRead = resolve; }));
    const { result } = renderHook(() => useSecurityLevel());
    await waitFor(() => expect(events.has('settings-preferences-changed')).toBe(true));
    policy = { ...initial, level: 'custom', session_timeout: -1 };
    act(() => events.get('settings-preferences-changed')({ payload: ['security'] }));
    expect(invoke.mock.calls.filter(([command]) => command === 'security_get_policy')).toHaveLength(1);
    await act(async () => resolveRead(initial));
    await waitFor(() => expect(result.current.policy).toEqual(policy));
    expect(result.current.loading).toBe(false);
  });
});
