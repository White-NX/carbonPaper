import React from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import TopBar from './TopBar';
import zhCN from '../i18n/locales/zh-CN.json';

vi.mock('./SearchBox', () => ({ SearchBox: () => null }));
vi.mock('react-i18next', () => ({
  useTranslation: () => ({ t: (key) => key.split('.').reduce((value, part) => value[part], zhCN) }),
}));

describe('capture status in the title bar', () => {
  it('explains power-saving offline status and restores the start action on AC', () => {
    const start = vi.fn();
    const { rerender } = render(<TopBar backendStatus="offline" powerSavingSuppressed handleStartBackend={start} />);
    const badge = screen.getByRole('button', { name: '因节电模式配置离线。' });
    expect(badge).toBeDisabled();
    expect(badge).toHaveAttribute('title', '因节电模式配置离线。');
    fireEvent.click(badge);
    expect(start).not.toHaveBeenCalled();

    rerender(<TopBar backendStatus="offline" powerSavingSuppressed={false} handleStartBackend={start} />);
    const offline = screen.getByRole('button', { name: '离线' });
    expect(offline).toBeEnabled();
    fireEvent.click(offline);
    expect(start).toHaveBeenCalledOnce();
  });

  it('keeps the running status while an admitted capture session drains', () => {
    const pause = vi.fn();
    render(<TopBar backendStatus="online" powerSavingSuppressed handlePauseMonitor={pause} />);
    fireEvent.click(screen.getByRole('button', { name: '运行中' }));
    expect(pause).toHaveBeenCalledOnce();
    expect(screen.queryByText('因节电模式配置离线。')).not.toBeInTheDocument();
  });
});
