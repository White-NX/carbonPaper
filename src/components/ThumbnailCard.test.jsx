import React from 'react';
import { act, render, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { fetchSearchImage, fetchThumbnail } from '../lib/monitor_api';
import { ThumbnailCard } from './ThumbnailCard';

vi.mock('../lib/monitor_api', () => ({
  fetchThumbnail: vi.fn(async () => 'thumbnail'),
  fetchSearchImage: vi.fn(async () => 'original'),
}));
vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));
afterEach(() => vi.unstubAllGlobals());

it('keeps thumbnails as the default for existing cards', async () => {
  const { container } = render(<ThumbnailCard item={{ screenshot_id: 1 }} />);
  await waitFor(() => expect(container.querySelector('img')).toHaveAttribute('src', 'thumbnail'));
  expect(fetchThumbnail).toHaveBeenCalledWith(1, null);
  expect(fetchSearchImage).not.toHaveBeenCalled();
});

it('uses the original for picture results without fetching or displaying a preloaded thumbnail', async () => {
  let observe;
  vi.stubGlobal('IntersectionObserver', class {
    constructor(callback) { observe = callback; }
    observe() {}
    disconnect() {}
  });
  const { container } = render(<ThumbnailCard item={{ screenshot_id: 2 }} imageKind="original" preloadedSrc="thumbnail" />);
  expect(container.querySelector('img')).not.toBeInTheDocument();
  expect(fetchThumbnail).not.toHaveBeenCalled();
  await act(async () => observe([{ isIntersecting: true }]));
  expect(container.querySelector('img')).toHaveAttribute('src', 'original');
  expect(fetchSearchImage).toHaveBeenCalledWith(2, null, { signal: expect.any(AbortSignal) });
});
