import React from 'react';
import { act, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { fetchSearchImage } from '../../lib/monitor_api';
import { OriginalSearchImage, SearchImageActivityContext } from './OriginalSearchImage';

vi.mock('../../lib/monitor_api', () => ({ fetchSearchImage: vi.fn() }));
vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key }) }));

let observers;
let requests;
beforeEach(() => {
  observers = [];
  requests = [];
  vi.stubGlobal('IntersectionObserver', class {
    constructor(callback) {
      this.callback = callback;
      this.disconnect = vi.fn();
      observers.push(this);
    }
    observe() {}
  });
  fetchSearchImage.mockReset().mockImplementation((id, path, { signal }) => new Promise((resolve, reject) => {
    requests.push({ id, path, signal, resolve, reject });
  }));
});
afterEach(() => vi.unstubAllGlobals());

const intersect = (visible, index = observers.length - 1) => act(() => observers[index].callback([{ isIntersecting: visible }]));

it('loads the original only near the viewport and does not repeat a visible request', async () => {
  const { container } = render(<OriginalSearchImage item={{ metadata: { screenshot_id: 12 }, image_path: 'memory://hash' }} />);
  expect(fetchSearchImage).not.toHaveBeenCalled();
  intersect(false);
  expect(fetchSearchImage).not.toHaveBeenCalled();
  intersect(true);
  intersect(true);
  expect(requests).toHaveLength(1);
  expect(requests[0]).toMatchObject({ id: 12, path: null });
  await act(async () => requests[0].resolve('data:image/jpeg;base64,original'));
  expect(container.querySelector('img')).toHaveAttribute('src', 'data:image/jpeg;base64,original');
});

it('retains an offscreen original and reuses it when the card returns', async () => {
  const { container } = render(<OriginalSearchImage item={{ screenshot_id: 13 }} />);
  intersect(true);
  await act(async () => requests[0].resolve('original'));
  expect(container.querySelector('img')).toBeInTheDocument();
  intersect(false);
  expect(requests[0].signal.aborted).toBe(false);
  expect(container.querySelector('img')).toHaveAttribute('src', 'original');
  intersect(true);
  expect(requests).toHaveLength(1);
  expect(container.querySelector('img')).toHaveAttribute('src', 'original');
});

it('lets the first read finish offscreen without restarting when the card returns', async () => {
  const { container } = render(<OriginalSearchImage item={{ screenshot_id: 14 }} />);
  intersect(true);
  intersect(false);
  expect(requests[0].signal.aborted).toBe(false);
  await act(async () => requests[0].resolve('offscreen result'));
  intersect(true);
  expect(requests).toHaveLength(1);
  expect(container.querySelector('img')).toHaveAttribute('src', 'offscreen result');
});

it('discards late reads after the result changes or the search closes', async () => {
  const { container, rerender, unmount } = render(<OriginalSearchImage item={{ screenshot_id: 14 }} />);
  intersect(true);
  rerender(<OriginalSearchImage item={{ screenshot_id: 15 }} />);
  expect(requests[0].signal.aborted).toBe(true);
  await act(async () => requests[0].resolve('late previous screenshot'));
  expect(container.querySelector('img')).not.toBeInTheDocument();
  intersect(true);
  unmount();
  expect(requests[1].signal.aborted).toBe(true);
  await act(async () => requests[1].resolve('late closed search'));
  expect(observers.every((observer) => observer.disconnect.mock.calls.length >= 1)).toBe(true);
});

it('keeps a loaded image when only the result object identity changes', async () => {
  const { container, rerender } = render(<OriginalSearchImage item={{ screenshot_id: 16 }} />);
  intersect(true);
  await act(async () => requests[0].resolve('original'));
  rerender(<OriginalSearchImage item={{ screenshot_id: 16, category: 'updated' }} />);
  expect(requests).toHaveLength(1);
  expect(container.querySelector('img')).toHaveAttribute('src', 'original');
});

it('supports path-only results and shows the existing placeholder after a read failure', async () => {
  render(<OriginalSearchImage item={{ image_path: 'memory://legacy' }} placeholder={<span>unavailable</span>} />);
  intersect(true);
  expect(requests[0]).toMatchObject({ id: null, path: 'memory://legacy' });
  await act(async () => requests[0].reject(new Error('missing image')));
  expect(screen.getByText('unavailable')).toBeInTheDocument();
});

it('releases originals and cancels pending reads while the search workspace is hidden', async () => {
  const View = ({ active }) => <SearchImageActivityContext.Provider value={active}><OriginalSearchImage item={{ screenshot_id: 18 }} /></SearchImageActivityContext.Provider>;
  const { container, rerender } = render(<View active />);
  intersect(true);
  await act(async () => requests[0].resolve('large original'));
  rerender(<View active={false} />);
  expect(container.querySelector('img')).not.toBeInTheDocument();
  rerender(<View active />);
  intersect(true);
  rerender(<View active={false} />);
  expect(requests[1].signal.aborted).toBe(true);
  await act(async () => requests[1].resolve('late hidden original'));
  expect(container.querySelector('img')).not.toBeInTheDocument();
});

it('avoids invalid requests and still loads when IntersectionObserver is unavailable', async () => {
  const { rerender } = render(<OriginalSearchImage item={{}} />);
  expect(screen.getByText('advancedSearch.no_image')).toBeInTheDocument();
  expect(fetchSearchImage).not.toHaveBeenCalled();
  vi.stubGlobal('IntersectionObserver', undefined);
  rerender(<OriginalSearchImage item={{ screenshot_id: 17 }} />);
  expect(requests[0].id).toBe(17);
  await act(async () => requests[0].resolve(null));
  expect(screen.getByText('advancedSearch.no_image')).toBeInTheDocument();
});
