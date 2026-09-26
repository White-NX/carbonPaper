import React from 'react';
import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { OverlayShell, ProgressBlock } from './index';
import { Banner } from '../ui/Banner';
import { formatError } from '../../lib/errors';
import { usePolling } from '../../hooks/usePolling';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));

describe('OverlayShell', () => {
  it('renders a labelled modal dialog', () => {
    render(<OverlayShell title="Title" subtitle="Sub"><p>Body</p></OverlayShell>);
    const dialog = screen.getByRole('dialog', { name: 'Title' });
    expect(dialog).toHaveAttribute('aria-modal', 'true');
    expect(dialog).toHaveTextContent('Body');
  });

  it('closes on Escape only when it is dismissable', () => {
    const onDismiss = vi.fn();
    const view = render(<OverlayShell title="A" onDismiss={onDismiss}><button type="button">x</button></OverlayShell>);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onDismiss).toHaveBeenCalledTimes(1);
    view.unmount();

    render(<OverlayShell title="B"><button type="button">x</button></OverlayShell>);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.getByRole('dialog', { name: 'B' })).toBeInTheDocument();
  });

  it('renders nothing while closed', () => {
    render(<OverlayShell open={false} title="Hidden" />);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('puts each layer in its own stacking band', () => {
    const { container, rerender } = render(<OverlayShell title="L" layer="maintenance" />);
    expect(container.firstChild.className).toContain('z-[70]');
    rerender(<OverlayShell title="L" layer="gate" />);
    expect(container.firstChild.className).toContain('z-[60]');
  });
});

describe('ProgressBlock', () => {
  it('derives the percentage from counts', () => {
    render(<ProgressBlock label="Working" current={20} total={80} />);
    expect(screen.getByText('20 / 80 (25%)')).toBeInTheDocument();
    expect(screen.getByRole('progressbar')).toHaveAttribute('aria-valuenow', '25');
  });

  it('is indeterminate without counts or a percentage', () => {
    render(<ProgressBlock label="Working" />);
    expect(screen.getByRole('progressbar')).not.toHaveAttribute('aria-valuenow');
  });
});

describe('Banner', () => {
  it('announces errors and nothing else', () => {
    const { rerender } = render(<Banner tone="info">note</Banner>);
    expect(screen.queryByRole('alert')).toBeNull();
    rerender(<Banner tone="error">broken</Banner>);
    expect(screen.getByRole('alert')).toHaveTextContent('broken');
  });
});

describe('formatError', () => {
  it('handles strings, errors, records and nothing', () => {
    expect(formatError('APP_BOUND_CANCELLED')).toBe('APP_BOUND_CANCELLED');
    expect(formatError(new Error('locked'))).toBe('locked');
    expect(formatError({ message: 'from record' })).toBe('from record');
    expect(formatError(null)).toBe('');
    expect(formatError(42)).toBe('42');
  });
});

describe('usePolling', () => {
  afterEach(() => vi.useRealTimers());

  function Poller({ tick, intervalMs = 1000, enabled = true }) {
    usePolling(tick, { intervalMs, enabled });
    return null;
  }

  it('ticks immediately, repeats, and stops when the tick returns false', async () => {
    vi.useFakeTimers();
    let calls = 0;
    const tick = vi.fn(async () => { calls += 1; return calls < 3; });
    render(<Poller tick={tick} />);
    await act(async () => { await vi.advanceTimersByTimeAsync(0); });
    expect(tick).toHaveBeenCalledTimes(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(tick).toHaveBeenCalledTimes(3);
  });

  it('keeps going after a failed tick', async () => {
    vi.useFakeTimers();
    const tick = vi.fn().mockRejectedValueOnce(new Error('busy')).mockResolvedValue(false);
    render(<Poller tick={tick} />);
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(tick).toHaveBeenCalledTimes(2);
  });

  it('does nothing while disabled', async () => {
    vi.useFakeTimers();
    const tick = vi.fn();
    render(<Poller tick={tick} enabled={false} />);
    await act(async () => { await vi.advanceTimersByTimeAsync(3000); });
    expect(tick).not.toHaveBeenCalled();
  });
});
