import React from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import TopBar from './TopBar';
import zhCN from '../i18n/locales/zh-CN.json';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({ t: (key) => key.split('.').reduce((value, part) => value[part], zhCN) }),
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => vi.fn()) }));

beforeEach(() => {
  invoke.mockImplementation(async (command) => command === 'storage_check_hmac_migration_status'
    ? { needs_migration: false, is_running: false }
    : null);
});

describe('capture status in the title bar', () => {
  it('explains power-saving offline status and restores the start action on AC', () => {
    const start = vi.fn();
    const { rerender } = render(<TopBar isAuthenticated backendStatus="offline" powerSavingSuppressed handleStartBackend={start} />);
    const badge = screen.getByRole('button', { name: '因节电模式配置离线。' });
    expect(badge).toBeDisabled();
    expect(badge).toHaveAttribute('title', '因节电模式配置离线。');
    fireEvent.click(badge);
    expect(start).not.toHaveBeenCalled();

    rerender(<TopBar isAuthenticated backendStatus="offline" powerSavingSuppressed={false} handleStartBackend={start} />);
    const offline = screen.getByRole('button', { name: '离线' });
    expect(offline).toBeEnabled();
    fireEvent.click(offline);
    expect(start).toHaveBeenCalledOnce();
  });

  it('keeps the running status while an admitted capture session drains', () => {
    const pause = vi.fn();
    render(<TopBar isAuthenticated backendStatus="online" powerSavingSuppressed handlePauseMonitor={pause} />);
    fireEvent.click(screen.getByRole('button', { name: '正在记录' }));
    expect(pause).toHaveBeenCalledOnce();
    expect(screen.queryByText('因节电模式配置离线。')).not.toBeInTheDocument();
  });

  it('accepts a quick-search query and disables capture and search while locked', () => {
    const submit = vi.fn(), pause = vi.fn();
    const { rerender } = render(<TopBar isAuthenticated backendStatus="online" onSearchSubmit={submit} handlePauseMonitor={pause} />);
    const input = screen.getByRole('textbox', { name: '搜索截图里出现过的文字' });
    fireEvent.change(input, { target: { value: 'design' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(submit).toHaveBeenCalledWith({ query: 'design', mode: 'ocr' });
    rerender(<TopBar isAuthenticated={false} backendStatus="online" onSearchSubmit={submit} handlePauseMonitor={pause} />);
    expect(input).toBeDisabled();
    expect(screen.getByRole('button', { name: '切换到画面搜索' })).toBeDisabled();
    expect(screen.getByRole('button', { name: '正在记录' })).toBeDisabled();

    rerender(<TopBar isAuthenticated interfaceLocked backendStatus="online" onSearchSubmit={submit} handlePauseMonitor={pause} />);
    expect(input).toBeDisabled();
    expect(screen.getByRole('button', { name: '正在记录' })).toBeDisabled();
  });
});
