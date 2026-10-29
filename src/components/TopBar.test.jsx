import React from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import TopBar from './TopBar';
import zhCN from '../i18n/locales/zh-CN.json';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({ t: (key) => key.split('.').reduce((value, part) => value[part], zhCN) }),
}));

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

  it('uses one search entry and disables capture and search while locked', () => {
    const search = vi.fn(), pause = vi.fn();
    const { rerender } = render(<TopBar isAuthenticated backendStatus="online" onOpenSearch={search} handlePauseMonitor={pause} />);
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: '搜索记录与回顾' }));
    expect(search).toHaveBeenCalledOnce();
    rerender(<TopBar isAuthenticated={false} backendStatus="online" onOpenSearch={search} handlePauseMonitor={pause} />);
    expect(screen.getByRole('button', { name: '搜索记录与回顾' })).toBeDisabled();
    expect(screen.getByRole('button', { name: '正在记录' })).toBeDisabled();
  });
});
