import React from 'react';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi, beforeEach } from 'vitest';
import { invoke } from '@tauri-apps/api/core';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key) => key,
  }),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

vi.mock('../lib/monitor_api', () => ({
  searchScreenshots: vi.fn(async () => []),
  fetchThumbnailBatch: vi.fn(async () => ({})),
  getSmartClusterWorkerStatus: vi.fn(async () => ({
    pending_count: 0,
    running: false,
    forceRunning: false,
    manualActive: false,
    phase: 'idle',
    total: 0,
    processed: 0,
  })),
  fetchThumbnail: vi.fn(async () => null),
  fetchImage: vi.fn(async () => null),
  fetchSearchImage: vi.fn(async () => 'data:image/jpeg;base64,original'),
  getBackgroundIndexProgress: vi.fn(async () => ({
    semantic: { running: false, processed: 0, indexed: 0, total: 0 },
    clip: { running: false, processed: 0, indexed: 0, total: 0 },
  })),
}));

import { SearchBox } from './SearchBox';
import { fetchSearchImage, fetchThumbnail, fetchThumbnailBatch, searchScreenshots } from '../lib/monitor_api';

afterEach(() => vi.unstubAllGlobals());

describe('SearchBox', () => {
  beforeEach(() => {
    invoke.mockImplementation((command) => {
      if (command === 'storage_check_hmac_migration_status') {
        return Promise.resolve({ needs_migration: false, is_running: false });
      }
      return Promise.resolve(null);
    });
  });

  it('submits query and mode on Enter', async () => {
    const onSubmit = vi.fn();

    render(
      <SearchBox
        onSelectResult={vi.fn()}
        onSubmit={onSubmit}
      />
    );

    const input = screen.getByPlaceholderText('search.placeholder.ocr');
    await userEvent.type(input, 'invoice{enter}');

    expect(onSubmit).toHaveBeenCalledWith({ query: 'invoice', mode: 'ocr' });
  });

  it('keeps controlled visual mode available when the monitor is offline', async () => {
    render(
      <SearchBox
        onSelectResult={vi.fn()}
        mode="nl"
      />
    );

    expect(screen.getByPlaceholderText('search.placeholder.nl')).toBeInTheDocument();
  });

  it('loads visible picture-search results as originals without requesting thumbnails', async () => {
    const observers = [];
    vi.stubGlobal('IntersectionObserver', class {
      constructor(callback) { observers.push(callback); }
      observe() {}
      disconnect() {}
    });
    searchScreenshots.mockResolvedValueOnce([{ metadata: { screenshot_id: 42, process_name: 'picture-source' } }]);
    const { container } = render(<SearchBox mode="nl" onSelectResult={vi.fn()} />);
    await userEvent.type(screen.getByPlaceholderText('search.placeholder.nl'), 'moon');
    await screen.findByText('picture-source');
    expect(fetchSearchImage).not.toHaveBeenCalled();
    expect(fetchThumbnailBatch).not.toHaveBeenCalled();
    await act(async () => observers.forEach((observer) => observer([{ isIntersecting: true }])));
    expect(fetchSearchImage).toHaveBeenCalledWith(42, null, { signal: expect.any(AbortSignal) });
    expect(container.querySelector('img')).toHaveAttribute('src', 'data:image/jpeg;base64,original');
    expect(fetchThumbnail).not.toHaveBeenCalled();
  });

  it('keeps text-search results on their existing thumbnail batch', async () => {
    searchScreenshots.mockResolvedValueOnce([{ screenshot_id: 43, process_name: 'text-source' }]);
    fetchThumbnailBatch.mockResolvedValueOnce({ 43: 'data:image/jpeg;base64,thumbnail' });
    const { container } = render(<SearchBox onSelectResult={vi.fn()} />);
    await userEvent.type(screen.getByPlaceholderText('search.placeholder.ocr'), 'invoice');
    await screen.findByText('text-source');
    await waitFor(() => expect(container.querySelector('img')).toHaveAttribute('src', 'data:image/jpeg;base64,thumbnail'));
    expect(fetchThumbnailBatch).toHaveBeenCalledWith([43]);
    expect(fetchSearchImage).not.toHaveBeenCalled();
  });

  it('releases a loaded original immediately when the picture query is cleared', async () => {
    const observers = [];
    vi.stubGlobal('IntersectionObserver', class {
      constructor(callback) { observers.push(callback); }
      observe() {}
      disconnect() {}
    });
    searchScreenshots.mockResolvedValueOnce([{ metadata: { screenshot_id: 44, process_name: 'loaded-picture' } }]);
    const { container } = render(<SearchBox mode="nl" onSelectResult={vi.fn()} />);
    const input = screen.getByPlaceholderText('search.placeholder.nl');
    await userEvent.type(input, 'moon');
    await screen.findByText('loaded-picture');
    await act(async () => observers.forEach((observer) => observer([{ isIntersecting: true }])));
    expect(container.querySelector('img')).toBeInTheDocument();
    const signal = fetchSearchImage.mock.calls[0][2].signal;
    fireEvent.change(input, { target: { value: '' } });
    expect(container.querySelector('img')).not.toBeInTheDocument();
    expect(signal.aborted).toBe(true);
  });

  it('does not restore old picture results when a search finishes after clearing its query', async () => {
    let finish;
    searchScreenshots.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    render(<SearchBox mode="nl" onSelectResult={vi.fn()} />);
    const input = screen.getByPlaceholderText('search.placeholder.nl');
    await userEvent.type(input, 'moon');
    await waitFor(() => expect(finish).toEqual(expect.any(Function)));
    fireEvent.change(input, { target: { value: '' } });
    await act(async () => finish([{ metadata: { screenshot_id: 45, process_name: 'late-picture' } }]));
    expect(screen.queryByText('late-picture')).not.toBeInTheDocument();
    expect(fetchSearchImage).not.toHaveBeenCalled();
  });

  it('cancels an original read when closing the picture results and ignores its late response', async () => {
    const observers = [];
    vi.stubGlobal('IntersectionObserver', class {
      constructor(callback) { observers.push(callback); }
      observe() {}
      disconnect() {}
    });
    let finishImage;
    fetchSearchImage.mockImplementationOnce(() => new Promise((resolve) => { finishImage = resolve; }));
    searchScreenshots.mockResolvedValueOnce([{ metadata: { screenshot_id: 46, process_name: 'pending-picture' } }]);
    const { container } = render(<SearchBox mode="nl" onSelectResult={vi.fn()} />);
    await userEvent.type(screen.getByPlaceholderText('search.placeholder.nl'), 'moon');
    await screen.findByText('pending-picture');
    await act(async () => observers.forEach((observer) => observer([{ isIntersecting: true }])));
    const signal = fetchSearchImage.mock.calls[0][2].signal;
    fireEvent.mouseDown(document.body);
    expect(signal.aborted).toBe(true);
    await act(async () => finishImage('data:image/jpeg;base64,late'));
    expect(screen.queryByText('pending-picture')).not.toBeInTheDocument();
    expect(container.querySelector('img')).not.toBeInTheDocument();
  });

  it('displays search error message when search fails', async () => {
    const consoleErrorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    const { searchScreenshots } = await import('../lib/monitor_api');
    searchScreenshots.mockRejectedValueOnce(new Error('Database locked'));

    try {
      render(
        <SearchBox
          onSelectResult={vi.fn()}
        />
      );

      const input = screen.getByPlaceholderText('search.placeholder.ocr');
      await userEvent.type(input, 'invoice');

      await waitFor(() => {
        expect(screen.getByText('search.searchError')).toBeInTheDocument();
      });
      expect(
        screen.queryByRole('button', { name: /search\.pressEnterToViewMore/i })
      ).not.toBeInTheDocument();
      expect(consoleErrorSpy).toHaveBeenCalledWith('Search failed:', expect.any(Error));
    } finally {
      consoleErrorSpy.mockRestore();
    }
  });

  it('renders "pressEnterToViewMore" hint bar and triggers onSubmit on click', async () => {
    const onSubmit = vi.fn();

    render(
      <SearchBox
        onSelectResult={vi.fn()}
        onSubmit={onSubmit}
      />
    );

    const input = screen.getByPlaceholderText('search.placeholder.ocr');
    await userEvent.type(input, 'deepseek');

    const hintButton = await screen.findByRole('button', { name: /search\.pressEnterToViewMore/i });
    expect(hintButton).toBeInTheDocument();

    await userEvent.click(hintButton);
    expect(onSubmit).toHaveBeenCalledWith({ query: 'deepseek', mode: 'ocr' });
  });
});
