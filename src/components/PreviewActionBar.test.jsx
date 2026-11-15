import React from 'react';
import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import PreviewActionBar from './PreviewActionBar';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} });
  vi.stubGlobal('matchMedia', () => ({ matches: false }));
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

function renderBar() {
  render(<PreviewActionBar selectedEvent={{ id: 42 }} />);
  const bar = screen.getByRole('toolbar');
  act(() => vi.advanceTimersByTime(2400));
  return { bar, dock: bar.parentElement };
}

describe('preview action bar reveal', () => {
  it('keeps both animation surfaces mounted while exposing only the active controls', () => {
    const { bar, dock } = renderBar();
    const reveal = screen.getByRole('button', { name: 'snapshotEditor.expandActions' });
    expect(bar).toBeInTheDocument();
    expect(bar).not.toHaveAttribute('hidden');
    expect(bar).toHaveAttribute('aria-hidden', 'true');
    expect(bar.inert).toBe(true);

    fireEvent.mouseEnter(reveal);
    expect(bar).toHaveAttribute('aria-hidden', 'false');
    expect(bar.inert).toBe(false);
    expect(reveal.parentElement.inert).toBe(true);

    fireEvent.mouseLeave(dock);
    expect(bar).toHaveAttribute('aria-hidden', 'true');
    expect(reveal).toBeInTheDocument();
    expect(reveal.parentElement.inert).toBe(false);
  });

  it('moves keyboard focus into the revealed toolbar and keeps it open while focused', () => {
    const { bar } = renderBar();
    fireEvent.click(screen.getByRole('button', { name: 'snapshotEditor.expandActions' }));
    act(() => vi.advanceTimersByTime(32));
    expect(bar.contains(document.activeElement)).toBe(true);

    act(() => vi.advanceTimersByTime(2400));
    expect(bar.inert).toBe(false);
    act(() => document.activeElement.blur());
    expect(bar.inert).toBe(true);
  });
});
