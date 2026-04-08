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
  fetchImage: vi.fn(async () => null),
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
        backendOnline
      />
    );

    const input = screen.getByPlaceholderText('search.placeholder.ocr');
    await userEvent.type(input, 'invoice{enter}');

    expect(onSubmit).toHaveBeenCalledWith({ query: 'invoice', mode: 'ocr' });
  });

  it('forces controlled mode back to ocr when backend is offline', async () => {
    const onModeChange = vi.fn();

    render(
      <SearchBox
        onSelectResult={vi.fn()}
        mode="nl"
        onModeChange={onModeChange}
        backendOnline={false}
      />
    );

    await waitFor(() => {
      expect(onModeChange).toHaveBeenCalledWith('ocr');
    });
  });
});
