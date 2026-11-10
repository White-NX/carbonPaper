import React from 'react';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../../lib/auth_api';
import UnattendedCredentialsCard from './UnattendedCredentialsCard';

vi.mock('react-i18next', async () => {
  const { createInstance } = await import('i18next');
  const { default: translation } = await import('../../../i18n/locales/zh-CN.json');
  const i18n = createInstance();
  await i18n.init({ lng: 'zh-CN', resources: { 'zh-CN': { translation } } });
  const t = i18n.getFixedT('zh-CN');
  return { useTranslation: () => ({ t, i18n }) };
});
vi.mock('../../../lib/auth_api', () => ({ withAuth: vi.fn((operation) => operation()) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('../../../lib/settings_api', () => ({ openSettingsWindow: vi.fn(async () => {}) }));
let status;
let issueError;
let clipboard;
const grant = { id: 'grant-a', label: 'Agent A', created_at_ms: 1000, expires_at_ms: null, last_used_at_ms: null, revoked: false };
beforeEach(() => {
  status = { enabled: true, available: true, phase: 'ready', preparation: {}, grants: [grant, { ...grant, id: 'grant-b', label: 'Agent B' }] };
  issueError = null;
  clipboard = vi.fn(async () => {});
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: clipboard } });
  invoke.mockReset().mockImplementation(async (command, args) => {
    if (command === 'agent_access_get_status') return status;
    if (command === 'agent_access_issue_credential') {
      if (issueError) throw issueError;
      const next = { ...grant, id: 'new-grant', label: args.label, expires_at_ms: args.expiresAtMs };
      status = { ...status, grants: [...status.grants.map((item) => item.id === args.replaceGrantId ? { ...item, revoked: true } : item), next] };
      return { grant: next, copied_to_clipboard: true };
    }
    if (command === 'agent_access_revoke_credential') {
      status = { ...status, grants: status.grants.map((item) => item.id === args.grantId ? { ...item, revoked: true } : item) };
    }
  });
});

describe('unattended credential management', () => {
  it('requests one-time native delivery with a label and expiry, without copying a secret in JavaScript', async () => {
    render(<UnattendedCredentialsCard />);
    const label = await screen.findByRole('textbox', { name: '智能体名称' });
    await userEvent.type(label, 'Night agent');
    await userEvent.click(screen.getByRole('button', { name: '创建并复制凭据' }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('agent_access_issue_credential', expect.objectContaining({ label: 'Night agent', replaceGrantId: null, expiresAtMs: expect.any(Number) })));
    const args = invoke.mock.calls.find(([name]) => name === 'agent_access_issue_credential')[1];
    expect(args.expiresAtMs).toBeGreaterThan(Date.now());
    expect(withAuth).toHaveBeenCalledWith(expect.any(Function), { promptMode: 'action' });
    expect(await screen.findByText(/凭据已复制到 Windows 剪贴板/)).toBeInTheDocument();
    await waitFor(() => expect(label).toHaveValue(''));
    expect(clipboard).not.toHaveBeenCalled();
    expect(screen.queryByRole('button', { name: '复制现有凭据' })).not.toBeInTheDocument();
  });

  it('hands replacement to one native operation and leaves the other Agent active', async () => {
    render(<UnattendedCredentialsCard />);
    await screen.findByRole('textbox', { name: '智能体名称' });
    await userEvent.click(screen.getAllByRole('button', { name: '替换' })[0]);
    expect(screen.getByRole('textbox', { name: '智能体名称' })).toHaveValue('Agent A');
    expect(screen.getByText(/原凭据随即失效/)).toBeInTheDocument();
    await userEvent.selectOptions(screen.getByRole('combobox', { name: '有效期' }), 'never');
    await userEvent.click(screen.getByRole('button', { name: '替换并复制凭据' }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('agent_access_issue_credential', { label: 'Agent A', expiresAtMs: null, replaceGrantId: 'grant-a' }));
    await screen.findByText('新凭据已复制，原凭据已撤销。');
    expect(status.grants.find((item) => item.id === 'grant-b').revoked).toBe(false);
    expect(invoke.mock.calls.some(([name]) => name === 'agent_access_revoke_credential')).toBe(false);
  });

  it.each(['preparing', 'syncing', 'reconciling'])('creates and replaces credentials during %s with a readiness explanation', async (phase) => {
    status = { ...status, phase, preparation: { processed: 20, total: 100 } };
    render(<UnattendedCredentialsCard />);
    const label = await screen.findByRole('textbox', { name: '智能体名称' });
    expect(screen.getByText(/文本历史准备完成后，凭据即可使用/)).toBeInTheDocument();
    await userEvent.type(label, 'Configured early');
    await userEvent.click(screen.getByRole('button', { name: '创建并复制凭据' }));
    await screen.findByText(/凭据已复制到 Windows 剪贴板/);
    expect(invoke).toHaveBeenCalledWith('agent_access_issue_credential', expect.objectContaining({ label: 'Configured early', replaceGrantId: null }));
    await waitFor(() => expect(label).toHaveValue(''));

    await userEvent.click(screen.getAllByRole('button', { name: '替换' })[0]);
    await userEvent.click(screen.getByRole('button', { name: '替换并复制凭据' }));
    await screen.findByText('新凭据已复制，原凭据已撤销。');
    expect(invoke).toHaveBeenCalledWith('agent_access_issue_credential', expect.objectContaining({ label: 'Agent A', replaceGrantId: 'grant-a' }));
    expect(status.phase).toBe(phase);
    expect(status.grants.find((item) => item.id === 'grant-b').revoked).toBe(false);
    expect(clipboard).not.toHaveBeenCalled();
  });

  it('keeps creation and replacement unavailable when protected components are unavailable', async () => {
    status = { ...status, phase: 'preparing', available: false };
    render(<UnattendedCredentialsCard />);
    await screen.findByRole('button', { name: '安全设置' });
    await waitFor(() => expect(screen.getAllByRole('button', { name: '替换' })[0]).toBeDisabled());
    expect(screen.queryByRole('textbox', { name: '智能体名称' })).not.toBeInTheDocument();
    expect(invoke.mock.calls.some(([name]) => name === 'agent_access_issue_credential')).toBe(false);
  });

  it('cancels revocation without changing grants and confirms only the selected credential', async () => {
    render(<UnattendedCredentialsCard />);
    await screen.findByRole('textbox', { name: '智能体名称' });
    await userEvent.click(screen.getAllByRole('button', { name: '撤销' })[0]);
    expect(screen.getByRole('dialog')).toHaveTextContent('Agent A');
    await userEvent.click(screen.getByRole('button', { name: '取消' }));
    expect(invoke.mock.calls.some(([name]) => name === 'agent_access_revoke_credential')).toBe(false);
    await userEvent.click(screen.getAllByRole('button', { name: '撤销' })[0]);
    await userEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: '撤销' }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('agent_access_revoke_credential', { grantId: 'grant-a' }));
    await screen.findByText('凭据已撤销，已交付给智能体的文字无法收回。');
    expect(status.grants.find((item) => item.id === 'grant-b').revoked).toBe(false);
  });

  it('retains an unfinished label and reports cancelled verification through subsequent refreshes', async () => {
    issueError = new Error('AUTH_CANCELLED');
    render(<UnattendedCredentialsCard />);
    const label = await screen.findByRole('textbox', { name: '智能体名称' });
    await userEvent.type(label, 'Night agent');
    await userEvent.click(screen.getByRole('button', { name: '创建并复制凭据' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('已取消');
    expect(label).toHaveValue('Night agent');
    expect(status.grants).toHaveLength(2);
    expect(clipboard).not.toHaveBeenCalled();
  });

  it('waits for prepared history and offers an explicit resume rather than credential creation', async () => {
    status = { ...status, phase: 'waiting_for_unlock' };
    render(<UnattendedCredentialsCard />);
    expect(await screen.findByRole('button', { name: '继续准备' })).toBeEnabled();
    expect(screen.queryByRole('textbox', { name: '智能体名称' })).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: '继续准备' }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('agent_access_resume'));
    expect(invoke.mock.calls.some(([name]) => name === 'agent_access_issue_credential')).toBe(false);
  });
});
