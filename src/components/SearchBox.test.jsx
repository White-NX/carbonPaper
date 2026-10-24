import React from 'react';
import { act, render, screen, waitFor } from '@testing-library/react';
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
