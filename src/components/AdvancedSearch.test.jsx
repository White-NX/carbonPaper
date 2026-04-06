import React from 'react';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { invoke } from '@tauri-apps/api/core';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key, options) => {
      if (options?.returnObjects) {
        return ['hint-a', 'hint-b'];
      }
      return key;
    },
  }),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

vi.mock('./ThumbnailCard', () => ({
  ThumbnailCard: ({ item, onSelect }) => (
    <button onClick={() => onSelect(item)}>{item.id || item.screenshot_id || 'thumb'}</button>
  ),
  CategoryBadge: () => <span>badge</span>,
}));

vi.mock('../lib/monitor_api', () => ({
  searchScreenshots: vi.fn(async () => []),
  fetchImage: vi.fn(async () => null),
  fetchThumbnailBatch: vi.fn(async () => ({})),
  listProcesses: vi.fn(async () => [{ process_name: 'code.exe', count: 2 }]),
  getCategoriesFromDb: vi.fn(async () => ['编程开发']),
  batchGetCategories: vi.fn(async () => ({})),
}));

import { AdvancedSearch } from './AdvancedSearch';
import { searchScreenshots, listProcesses, getCategoriesFromDb } from '../lib/monitor_api';

const makeOcrResult = (id) => ({
  screenshot_id: id,
  process_name: 'code.exe',
  text: `text-${id}`,
  window_title: `window-${id}`,
});

describe('AdvancedSearch', () => {
  let observerCallback;

  beforeEach(() => {
    observerCallback = null;
    globalThis.IntersectionObserver = class {
      constructor(callback) {
        observerCallback = callback;
      }

      observe() {}

      unobserve() {}

      disconnect() {}
    };

    invoke.mockImplementation((command) => {
      if (command === 'storage_check_hmac_migration_status') {
        return Promise.resolve({ needs_migration: false, is_running: false });
      }
      return Promise.resolve(null);
    });
  });

  it('loads filter options when active', async () => {
    render(
      <AdvancedSearch
        active
        searchParams={{ query: '', mode: 'ocr' }}
        onSelectResult={vi.fn()}
        backendOnline
      />
    );

    await waitFor(() => {
      expect(listProcesses).toHaveBeenCalledTimes(1);
      expect(getCategoriesFromDb).toHaveBeenCalledTimes(1);
    });
  });

  it('runs OCR search with debounced query from searchParams', async () => {
    searchScreenshots.mockResolvedValueOnce([{ screenshot_id: 1, text: 'hello world' }]);

    render(
      <AdvancedSearch
        active
        searchParams={{ query: 'hello', mode: 'ocr' }}
        onSelectResult={vi.fn()}
        backendOnline
      />
    );

    await waitFor(() => {
      expect(searchScreenshots).toHaveBeenCalledWith('hello', 'ocr', expect.objectContaining({
        limit: 40,
        fuzzy: true,
      }));
    });
  });

  it('auto-switches to OCR when backend is offline in NL mode', async () => {
    const onSearchModeChange = vi.fn();

    render(
      <AdvancedSearch
        active
        searchParams={{ query: 'q', mode: 'nl' }}
        searchMode="nl"
        onSearchModeChange={onSearchModeChange}
        onSelectResult={vi.fn()}
        backendOnline={false}
      />
    );

    await waitFor(() => {
      expect(onSearchModeChange).toHaveBeenCalledWith('ocr');
    });
  });

  it('shows no-result state when query exists but no matches', async () => {
    searchScreenshots.mockResolvedValueOnce([]);

    render(
      <AdvancedSearch
        active
        searchParams={{ query: 'missing', mode: 'ocr' }}
        onSelectResult={vi.fn()}
        backendOnline
      />
    );

    await waitFor(() => {
      expect(screen.getByText('advancedSearch.search.no_results')).toBeInTheDocument();
    });
  });

  it('sends process/category/time filters in OCR search options', async () => {
    searchScreenshots.mockResolvedValue([]);

    render(
      <AdvancedSearch
        active
        searchParams={{ query: '', mode: 'ocr' }}
        onSelectResult={vi.fn()}
        backendOnline
      />
    );

    await waitFor(() => {
      expect(listProcesses).toHaveBeenCalledTimes(1);
    });

    fireEvent.click(screen.getByText('advancedSearch.processes.all'));
    const processCheckbox = screen.getByText('code.exe').closest('label')?.querySelector('input[type="checkbox"]');
    expect(processCheckbox).not.toBeNull();
    fireEvent.click(processCheckbox);

    fireEvent.click(screen.getByText('advancedSearch.categories.all'));
    const categoryCheckbox = screen.getByText('编程开发').closest('label')?.querySelector('input[type="checkbox"]');
    expect(categoryCheckbox).not.toBeNull();
    fireEvent.click(categoryCheckbox);

    const [startInput, endInput] = document.querySelectorAll('input[type="datetime-local"]');
    fireEvent.change(startInput, { target: { value: '2026-01-02T03:04' } });
    fireEvent.change(endInput, { target: { value: '2026-01-02T04:05' } });

    await waitFor(() => {
      expect(searchScreenshots).toHaveBeenCalledWith('', 'ocr', expect.objectContaining({
        processNames: ['code.exe'],
        categories: ['编程开发'],
        startTime: Math.floor(new Date('2026-01-02T03:04').getTime() / 1000),
        endTime: Math.floor(new Date('2026-01-02T04:05').getTime() / 1000),
      }));
    });
  });

  it('loads more results when sentinel intersects', async () => {
    searchScreenshots
      .mockResolvedValueOnce(Array.from({ length: 40 }, (_, i) => makeOcrResult(i + 1)))
      .mockResolvedValueOnce([makeOcrResult(41)]);

    render(
      <AdvancedSearch
        active
        searchParams={{ query: 'page', mode: 'ocr' }}
        onSelectResult={vi.fn()}
        backendOnline
      />
    );

    await waitFor(() => {
      expect(searchScreenshots).toHaveBeenCalledWith('page', 'ocr', expect.objectContaining({
        limit: 40,
        offset: 0,
      }));
    });

    await act(async () => {
      observerCallback?.([{ isIntersecting: true }]);
    });

    await waitFor(() => {
      expect(searchScreenshots).toHaveBeenCalledWith('page', 'ocr', expect.objectContaining({
        limit: 40,
        offset: 40,
      }));
    });

    expect(screen.getByText('text-41')).toBeInTheDocument();
  });
});
