import React from 'react';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi, beforeEach } from 'vitest';
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
  getBackgroundIndexProgress: vi.fn(async () => ({
    semantic: { running: false, processed: 0, indexed: 0, total: 0 },
    clip: { running: false, processed: 0, indexed: 0, total: 0 },
  })),
}));

import { SearchBox } from './SearchBox';

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
