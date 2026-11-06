import React from 'react';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('../lib/monitor_api', () => ({
  searchScreenshots: vi.fn(), fetchThumbnailBatch: vi.fn(),
  getSmartClusterWorkerStatus: vi.fn(async () => ({ pending_count: 0, running: false, phase: 'idle' })),
  normalizeSmartClusterWorkerStatus: (value) => value,
  fetchThumbnail: vi.fn(async () => null), fetchSearchImage: vi.fn(async () => null),
  getBackgroundIndexProgress: vi.fn(async () => ({
    semantic: { running: false, total: 0 }, clip: { running: false, total: 0 },
  })),
}));

import { SearchBox } from './SearchBox';
import { fetchSearchImage, fetchThumbnailBatch, searchScreenshots } from '../lib/monitor_api';

const screenshot = { screenshot_id: 42, image_path: 'memory://42', process_name: 'text-source', text: 'invoice text' };

describe('SearchBox', () => {
  beforeEach(() => {
    searchScreenshots.mockReset().mockResolvedValue([]);
    fetchThumbnailBatch.mockReset().mockResolvedValue({ 42: 'data:image/jpeg;base64,thumbnail' });
    invoke.mockImplementation((command) => command === 'storage_check_hmac_migration_status'
      ? Promise.resolve({ needs_migration: false, is_running: false }) : Promise.resolve(null));
  });

  it('submits the query to unified search on Enter', async () => {
    const onSubmit = vi.fn();
    render(<SearchBox onSelectResult={vi.fn()} onSubmit={onSubmit} />);
    await userEvent.type(screen.getByPlaceholderText('search.placeholder.unified'), 'invoice{enter}');
    expect(onSubmit).toHaveBeenCalledWith({ query: 'invoice' });
  });

  it('shows one search input without mode controls', async () => {
    render(<SearchBox onSelectResult={vi.fn()} />);
    await act(async () => {});
    expect(screen.getByPlaceholderText('search.placeholder.unified')).toBeInTheDocument();
    expect(screen.queryByTitle('search.selectMode')).not.toBeInTheDocument();
    expect(screen.queryByTitle('search.switchToNL')).not.toBeInTheDocument();
  });

  it('keeps quick text results on thumbnails and opens the selected screenshot', async () => {
    searchScreenshots.mockResolvedValueOnce([screenshot]);
    const onSelectResult = vi.fn();
    const { container } = render(<SearchBox onSelectResult={onSelectResult} />);
    await userEvent.type(screen.getByPlaceholderText('search.placeholder.unified'), 'invoice');
    await screen.findByText('text-source');
    await waitFor(() => expect(container.querySelector('img')).toHaveAttribute('src', 'data:image/jpeg;base64,thumbnail'));
    expect(fetchThumbnailBatch).toHaveBeenCalledWith([42]);
    expect(fetchSearchImage).not.toHaveBeenCalled();
    expect(searchScreenshots).toHaveBeenCalledWith('invoice', 'ocr', { signal: expect.any(AbortSignal) });
    fireEvent.click(screen.getByText('text-source'));
    expect(onSelectResult).toHaveBeenCalledWith(expect.objectContaining({ id: 42, path: 'memory://42' }));
  });

  it('clears quick results immediately when the input is emptied', async () => {
    searchScreenshots.mockResolvedValueOnce([screenshot]);
    const { container } = render(<SearchBox onSelectResult={vi.fn()} />);
    const input = screen.getByPlaceholderText('search.placeholder.unified');
    await userEvent.type(input, 'invoice');
    await screen.findByText('text-source');
    fireEvent.change(input, { target: { value: '' } });
    expect(screen.queryByText('text-source')).not.toBeInTheDocument();
    expect(container.querySelector('img')).not.toBeInTheDocument();
  });

  it.each(['clear', 'disable'])('discards a late search response after %s', async (action) => {
    let finishSearch;
    searchScreenshots.mockImplementationOnce(() => new Promise((resolve) => { finishSearch = resolve; }));
    const { rerender } = render(<SearchBox onSelectResult={vi.fn()} />);
    const input = screen.getByPlaceholderText('search.placeholder.unified');
    await userEvent.type(input, 'invoice');
    await waitFor(() => expect(finishSearch).toEqual(expect.any(Function)));
    const signal = searchScreenshots.mock.calls[0][2].signal;
    if (action === 'clear') fireEvent.change(input, { target: { value: '' } });
    else rerender(<SearchBox disabled onSelectResult={vi.fn()} />);
    expect(signal.aborted).toBe(true);
    await act(async () => finishSearch([screenshot]));
    expect(screen.queryByText('text-source')).not.toBeInTheDocument();
  });

  it('shows a quick-search error without opening a visual request', async () => {
    const quiet = vi.spyOn(console, 'error').mockImplementation(() => {});
    searchScreenshots.mockRejectedValueOnce(new Error('Database locked'));
    try {
      render(<SearchBox onSelectResult={vi.fn()} />);
      await userEvent.type(screen.getByPlaceholderText('search.placeholder.unified'), 'invoice');
      await screen.findByText('search.searchError');
      expect(searchScreenshots.mock.calls.every(([, mode]) => mode === 'ocr')).toBe(true);
    } finally { quiet.mockRestore(); }
  });

  it('opens unified search from the results footer', async () => {
    searchScreenshots.mockResolvedValueOnce([screenshot]);
    const onSubmit = vi.fn();
    render(<SearchBox onSelectResult={vi.fn()} onSubmit={onSubmit} />);
    await userEvent.type(screen.getByPlaceholderText('search.placeholder.unified'), 'invoice');
    fireEvent.click(await screen.findByRole('button', { name: 'search.pressEnterToViewMore' }));
    expect(onSubmit).toHaveBeenCalledWith({ query: 'invoice' });
  });

  it('keeps late quick results closed after submission until the input is focused again', async () => {
    let finishSearch;
    searchScreenshots.mockImplementationOnce(() => new Promise((resolve) => { finishSearch = resolve; }));
    const onSubmit = vi.fn();
    render(<SearchBox onSelectResult={vi.fn()} onSubmit={onSubmit} />);
    const input = screen.getByPlaceholderText('search.placeholder.unified');
    await userEvent.type(input, 'invoice');
    await waitFor(() => expect(finishSearch).toEqual(expect.any(Function)));
    fireEvent.keyDown(input, { key: 'Enter' });
    await act(async () => finishSearch([screenshot]));
    expect(onSubmit).toHaveBeenCalledWith({ query: 'invoice' });
    expect(screen.queryByText('text-source')).not.toBeInTheDocument();
    fireEvent.focus(input);
    expect(screen.getByText('text-source')).toBeInTheDocument();
  });
});
