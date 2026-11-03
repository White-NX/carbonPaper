import React from 'react';
import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { VisualSearchCard } from './VisualSearchCard';
import { fetchSearchImage, fetchThumbnail } from '../../lib/monitor_api';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key, options) =>
  key === 'advancedSearch.visual.relevance' ? `${key}:${options.score}` : key }) }));
vi.mock('../../lib/monitor_api', () => ({ fetchSearchImage: vi.fn(), fetchThumbnail: vi.fn() }));
vi.mock('../../lib/preference_store', () => ({ usePreference: () => 'preview' }));

const screenshot = { id: 42, screenshot_id: 42, image_path: 'memory://42', process_name: 'code.exe' };
const controller = (overrides = {}) => ({ phase: 'ready', results: [screenshot], summaryResults: [screenshot],
  thumbnails: { 42: 'data:image/png;base64,preview' }, hasMore: false, summaryHasMore: false,
  summaryExpanded: false, loadingMore: false, pageError: false, pageErrorMessage: null, summaryPageError: false,
  galleryReady: true, retry: vi.fn(), loadMore: vi.fn(), loadGalleryPage: vi.fn(), showMore: vi.fn(), ...overrides });

afterEach(() => vi.unstubAllGlobals());

describe('visual search results', () => {
  let observers;
  beforeEach(() => {
    fetchSearchImage.mockReset(); fetchThumbnail.mockReset();
    observers = [];
    vi.stubGlobal('IntersectionObserver', class {
      constructor(callback, options) { observers.push({ callback, options }); }
      observe() {}
      disconnect() {}
    });
  });

  it.each([false, true])('shows the pending index notice during retrieval and removes it when ready (gallery=%s)', (fullPage) => {
    const state = controller({ phase: 'loading', results: [], summaryResults: [], indexReady: false });
    const { rerender } = render(<VisualSearchCard controller={state} fullPage={fullPage} />);
    expect(screen.getByRole('status')).toHaveTextContent('advancedSearch.visual.index_pending');
    expect(screen.getByRole('region')).toHaveAttribute('aria-busy', 'true');
    rerender(<VisualSearchCard controller={{ ...state, indexReady: true }} fullPage={fullPage} />);
    expect(screen.queryByText('advancedSearch.visual.index_pending')).not.toBeInTheDocument();
  });

  it('shows inline results, loads only visible originals, and opens the gallery from the title', async () => {
    fetchSearchImage.mockResolvedValue('data:image/jpeg;base64,original');
    const onSelectResult = vi.fn(), onOpenGallery = vi.fn();
    render(<VisualSearchCard controller={controller()} onSelectResult={onSelectResult} onOpenGallery={onOpenGallery} />);
    const region = screen.getByRole('region', { name: 'advancedSearch.visual.results' });
    expect(region.querySelector('img')).not.toBeInTheDocument();
    expect(fetchSearchImage).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'advancedSearch.visual.open' }));
    expect(onOpenGallery).toHaveBeenCalledOnce();
    await act(async () => observers.forEach(({ callback }) => callback([{ isIntersecting: true }])));
    expect(fetchSearchImage).toHaveBeenCalledWith(42, null, { signal: expect.any(AbortSignal) });
    expect(region.querySelector('img')).toHaveAttribute('src', 'data:image/jpeg;base64,original');
    fireEvent.click(within(region).getAllByRole('button')[0]);
    expect(onSelectResult).toHaveBeenCalledWith(expect.objectContaining({ id: 42, path: 'memory://42' }));
    expect(fetchThumbnail).not.toHaveBeenCalled();
  });

  it('expands the summary once and changes the capsule into a gallery action', () => {
    const results = Array.from({ length: 30 }, (_, index) => ({ ...screenshot, id: index + 1, screenshot_id: index + 1 }));
    const state = controller({ results, summaryResults: results.slice(0, 10), summaryHasMore: true });
    const onOpenGallery = vi.fn();
    const { rerender } = render(<VisualSearchCard controller={state} onOpenGallery={onOpenGallery} />);
    const region = screen.getByRole('region');
    expect(within(region).getAllByRole('button')).toHaveLength(11);
    const more = screen.getByRole('button', { name: 'advancedSearch.visual.more' });
    expect(more).toHaveAttribute('aria-controls', region.id);
    fireEvent.click(more);
    expect(state.showMore).toHaveBeenCalledOnce();
    expect(state.loadMore).not.toHaveBeenCalled();
    rerender(<VisualSearchCard controller={{ ...state, summaryResults: results.slice(0, 20),
      summaryExpanded: true, summaryHasMore: false }} onOpenGallery={onOpenGallery} />);
    expect(within(region).getAllByRole('button')).toHaveLength(21);
    expect(screen.queryByRole('button', { name: 'advancedSearch.visual.more' })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'advancedSearch.visual.view_more' }));
    expect(onOpenGallery).toHaveBeenCalledOnce();
    expect(state.showMore).toHaveBeenCalledOnce();
  });

  it('aborts an original read when results are cleared and discards its late result', async () => {
    vi.stubGlobal('IntersectionObserver', undefined);
    let finishImage;
    fetchSearchImage.mockImplementationOnce(() => new Promise((resolve) => { finishImage = resolve; }));
    const state = controller();
    const { container, rerender } = render(<VisualSearchCard controller={state} />);
    const signal = fetchSearchImage.mock.calls[0][2].signal;
    rerender(<VisualSearchCard controller={{ ...state, results: [], summaryResults: [] }} />);
    expect(signal.aborted).toBe(true);
    await act(async () => finishImage('data:image/jpeg;base64,obsolete'));
    expect(container.querySelector('img')).not.toBeInTheDocument();
  });

  it('keeps cached summary paging available after a gallery error and retries through the same capsule', () => {
    const state = controller({ pageError: true, summaryHasMore: true });
    const { rerender } = render(<VisualSearchCard controller={state} />);
    expect(screen.queryByText('advancedSearch.visual.page_error')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'advancedSearch.visual.more' }));
    expect(state.showMore).toHaveBeenCalledOnce();
    rerender(<VisualSearchCard controller={{ ...state, summaryExpanded: true, summaryPageError: true,
      pageErrorMessage: 'temporary failure' }} />);
    expect(screen.getByRole('alert')).toHaveTextContent('advancedSearch.visual.page_error');
    expect(screen.getByRole('alert')).toHaveTextContent('temporary failure');
    fireEvent.click(screen.getByRole('button', { name: 'advancedSearch.visual.retry' }));
    expect(state.showMore).toHaveBeenCalledTimes(2);
    expect(state.retry).not.toHaveBeenCalled();
  });

  it('offers retry for a failed visual search', () => {
    const state = controller({ phase: 'error', results: [], summaryResults: [] });
    render(<VisualSearchCard controller={state} />);
    expect(screen.getByText('advancedSearch.visual.retry_hint')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'advancedSearch.visual.retry' }));
    expect(state.retry).toHaveBeenCalledOnce();
  });

  it('displays finite relevance scores to two decimals in both views', () => {
    const results = [0.832, 0, -0.17, undefined, NaN, Infinity].map((similarity, index) => ({
      ...screenshot, id: index + 1, screenshot_id: index + 1, similarity,
    }));
    const state = controller({ results, summaryResults: results });
    const { rerender } = render(<VisualSearchCard controller={state} />);
    expect(screen.getByText('advancedSearch.visual.relevance:0.83')).toBeInTheDocument();
    expect(screen.getByText('advancedSearch.visual.relevance:0.00')).toBeInTheDocument();
    expect(screen.getByText('advancedSearch.visual.relevance:-0.17')).toBeInTheDocument();
    expect(screen.getAllByText(/advancedSearch.visual.relevance:/)).toHaveLength(3);
    rerender(<VisualSearchCard controller={state} fullPage />);
    expect(screen.getAllByText(/advancedSearch.visual.relevance:/)).toHaveLength(3);
  });

  it('prepares the gallery first page before observing its next page', () => {
    const state = controller({ galleryReady: false, hasMore: true });
    const { rerender } = render(<VisualSearchCard controller={state} fullPage />);
    expect(state.loadGalleryPage).toHaveBeenCalledOnce();
    expect(observers.some(({ options }) => Object.hasOwn(options, 'root'))).toBe(false);
    rerender(<VisualSearchCard controller={{ ...state, galleryReady: true }} fullPage />);
    expect(observers.some(({ options }) => Object.hasOwn(options, 'root'))).toBe(true);
  });

  it('loads more on scrolling only in the gallery and stops automatic retries after a paging error', () => {
    const state = controller({ hasMore: true, summaryHasMore: true });
    const onBack = vi.fn();
    const { rerender } = render(<VisualSearchCard controller={state} onBack={onBack} />);
    expect(observers.some(({ options }) => Object.hasOwn(options, 'root'))).toBe(false);
    rerender(<VisualSearchCard controller={state} fullPage onBack={onBack} />);
    const paging = observers.find(({ options }) => Object.hasOwn(options, 'root'));
    act(() => paging.callback([{ isIntersecting: true }]));
    expect(state.loadGalleryPage).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole('button', { name: 'advancedSearch.visual.back' }));
    expect(onBack).toHaveBeenCalledOnce();
    const observerCount = observers.length;
    rerender(<VisualSearchCard controller={{ ...state, pageError: true }} fullPage onBack={onBack} />);
    expect(observers).toHaveLength(observerCount);
    fireEvent.click(screen.getByRole('button', { name: 'advancedSearch.visual.retry' }));
    expect(state.loadGalleryPage).toHaveBeenCalledTimes(2);
  });
});
