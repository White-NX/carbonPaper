import React from 'react';
import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { VisualSearchCard } from './VisualSearchCard';
import { fetchSearchImage, fetchThumbnail } from '../../lib/monitor_api';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));
vi.mock('../../lib/monitor_api', () => ({ fetchSearchImage: vi.fn(), fetchThumbnail: vi.fn() }));
vi.mock('../../lib/preference_store', () => ({ usePreference: () => 'preview' }));

const screenshot = { id: 42, screenshot_id: 42, image_path: 'memory://42', process_name: 'code.exe' };
const controller = (overrides = {}) => ({ phase: 'ready', results: [screenshot], thumbnails: { 42: 'data:image/png;base64,preview' },
  expanded: false, setExpanded: vi.fn(), hasMore: false, loadingMore: false, pageError: false,
  retry: vi.fn(), loadMore: vi.fn(), ...overrides });

afterEach(() => vi.unstubAllGlobals());

describe('visual search card', () => {
  beforeEach(() => { fetchSearchImage.mockReset(); fetchThumbnail.mockReset(); });

  it('shows a thumbnail preview while collapsed and loads originals only when expanded and visible', async () => {
    const observers = [];
    vi.stubGlobal('IntersectionObserver', class {
      constructor(callback) { observers.push(callback); }
      observe() {}
      disconnect() {}
    });
    fetchSearchImage.mockResolvedValue('data:image/jpeg;base64,original');
    const state = controller();
    const onSelectResult = vi.fn();
    const { rerender } = render(<VisualSearchCard controller={state} onSelectResult={onSelectResult} />);
    const disclosure = screen.getByRole('button', { name: 'advancedSearch.visual.title' });
    expect(disclosure).toHaveAttribute('aria-expanded', 'false');
    expect(document.getElementById(disclosure.getAttribute('aria-controls'))).toBeInTheDocument();
    expect(screen.queryByRole('region')).not.toBeInTheDocument();
    expect(fetchSearchImage).not.toHaveBeenCalled();
    fireEvent.click(disclosure);
    expect(state.setExpanded).toHaveBeenCalledWith(true);
    rerender(<VisualSearchCard controller={{ ...state, expanded: true }} onSelectResult={onSelectResult} />);
    expect(fetchSearchImage).not.toHaveBeenCalled();
    await act(async () => observers.forEach((observe) => observe([{ isIntersecting: true }])));
    expect(fetchSearchImage).toHaveBeenCalledWith(42, null, { signal: expect.any(AbortSignal) });
    const region = screen.getByRole('region');
    expect(region.querySelector('img')).toHaveAttribute('src', 'data:image/jpeg;base64,original');
    fireEvent.click(within(region).getAllByRole('button')[0]);
    expect(onSelectResult).toHaveBeenCalledWith(expect.objectContaining({ id: 42, path: 'memory://42' }));
    expect(fetchThumbnail).not.toHaveBeenCalled();
  });

  it('aborts an original read on collapse and discards its late result', async () => {
    vi.stubGlobal('IntersectionObserver', undefined);
    let finishImage;
    fetchSearchImage.mockImplementationOnce(() => new Promise((resolve) => { finishImage = resolve; }));
    const state = controller({ expanded: true });
    const { container, rerender } = render(<VisualSearchCard controller={state} />);
    const signal = fetchSearchImage.mock.calls[0][2].signal;
    rerender(<VisualSearchCard controller={{ ...state, expanded: false }} />);
    expect(signal.aborted).toBe(true);
    await act(async () => finishImage('data:image/jpeg;base64,obsolete'));
    expect(container.querySelector('img')).toHaveAttribute('src', 'data:image/png;base64,preview');
    expect(container.querySelector('img[src="data:image/jpeg;base64,obsolete"]')).not.toBeInTheDocument();
  });

  it('offers retry for a failed visual search', () => {
    const state = controller({ expanded: true, phase: 'error', results: [] });
    render(<VisualSearchCard controller={state} />);
    expect(screen.getByText('advancedSearch.visual.retry_hint')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'advancedSearch.visual.retry' }));
    expect(state.retry).toHaveBeenCalledOnce();
  });
});
