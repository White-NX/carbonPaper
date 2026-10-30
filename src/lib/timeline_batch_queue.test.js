import { afterEach, describe, expect, it, vi } from 'vitest';
import { createTimelineBatchQueue } from './timeline_batch_queue';

const flush = async () => { await Promise.resolve(); await Promise.resolve(); };
function fixture(loaded = []) {
  const calls = [];
  const cache = new Set(loaded);
  const load = vi.fn((ids, { signal }) => new Promise((resolve, reject) => {
    calls.push({ ids, signal, resolve, reject });
  }));
  const onResult = vi.fn((result) => {
    const accepted = Object.keys(result).map(Number);
    accepted.forEach((id) => cache.add(id));
    return accepted;
  });
  const onError = vi.fn();
  const queue = createTimelineBatchQueue(load, {
    batchSize: 2, isLoaded: (id) => cache.has(id), onResult, onError,
  });
  return { calls, queue, cache, onResult, onError };
}

afterEach(() => vi.useRealTimers());

describe('progressive timeline batches', () => {
  it('publishes each small batch before requesting the next without a throttle gap', async () => {
    const f = fixture([1]);
    f.queue.setIds([1, 2, 2, 3, 4, 5, 6, -1, Infinity, 1.5, '7'], { paused: false });
    expect(f.calls[0].ids).toEqual([2, 3]);
    f.calls[0].resolve({ 2: 'image', 3: 'image' });
    await flush();
    expect(f.onResult).toHaveBeenCalledWith({ 2: 'image', 3: 'image' }, [2, 3]);
    expect(f.calls[1].ids).toEqual([4, 5]);
    f.calls[1].resolve({ 4: 'image', 5: 'image' });
    await flush();
    expect(f.calls[2].ids).toEqual([6]);
    f.calls[2].resolve({ 6: 'image' });
    await flush();
    expect(f.queue.isBusy()).toBe(false);
    f.queue.setIds([6, 5, 4, 3, 2, 1]);
    expect(f.calls).toHaveLength(3);
  });

  it('retains the cancelled IPC slot and loads only the latest viewport after acknowledgement', async () => {
    const f = fixture();
    f.queue.setIds([1, 2, 3, 4], { paused: false });
    f.queue.setIds([10, 11, 12], { paused: false });
    f.queue.setIds([20, 21], { paused: false });
    expect(f.calls[0].signal.aborted).toBe(true);
    expect(f.calls).toHaveLength(1);
    f.calls[0].resolve({ 1: 'stale', 2: 'stale' });
    await flush();
    expect(f.onResult).not.toHaveBeenCalled();
    expect(f.calls[1].ids).toEqual([20, 21]);
    f.calls[1].resolve({ 20: 'current', 21: 'current' });
    await flush();
  });

  it('keeps an overlapping batch useful and applies the new priority to later batches', async () => {
    const f = fixture();
    f.queue.setIds([1, 2, 3, 4], { paused: false });
    f.queue.setIds([2, 4, 5], { paused: false });
    expect(f.calls[0].signal.aborted).toBe(false);
    f.calls[0].resolve({ 1: 'image', 2: 'image' });
    await flush();
    expect(f.calls[1].ids).toEqual([4, 5]);
    f.calls[1].resolve({ 4: 'image', 5: 'image' });
    await flush();
  });

  it('resumes with updated IDs atomically instead of admitting an old paused batch', async () => {
    const f = fixture();
    f.queue.setIds([1, 2, 3, 4], { paused: false });
    f.queue.setPaused(true);
    f.calls[0].resolve({ 1: 'stale' });
    await flush();
    expect(f.calls).toHaveLength(1);
    f.queue.setIds([10, 11], { paused: false });
    expect(f.calls[1].ids).toEqual([10, 11]);
    f.queue.setIds([12, 13], { paused: true });
    expect(f.calls[1].signal.aborted).toBe(true);
    f.calls[1].resolve({ 10: 'stale' });
    await flush();
    expect(f.calls).toHaveLength(2);
    f.queue.setPaused(false);
    expect(f.calls[2].ids).toEqual([12, 13]);
    f.calls[2].resolve({ 12: 'image', 13: 'image' });
    await flush();
  });

  it('discards results from before reset even when the same IDs are requested again', async () => {
    const f = fixture();
    f.queue.setIds([1, 2], { paused: false });
    f.queue.reset();
    f.queue.setIds([1, 2], { paused: false });
    expect(f.calls).toHaveLength(1);
    f.calls[0].resolve({ 1: 'old', 2: 'old' });
    await flush();
    expect(f.onResult).not.toHaveBeenCalled();
    expect(f.calls).toHaveLength(2);
    f.calls[1].resolve({ 1: 'new', 2: 'new' });
    await flush();
    expect(f.cache.has(1)).toBe(true);
  });

  it('retries only missing results after cooldown on a later viewport or refresh update', async () => {
    vi.useFakeTimers();
    const f = fixture();
    f.queue.setIds([1, 2], { paused: false });
    f.calls[0].resolve({ 1: 'image' });
    await flush();
    expect(f.queue.isBusy()).toBe(false);
    await vi.advanceTimersByTimeAsync(4999);
    f.queue.setIds([1, 2]);
    expect(f.calls).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(f.queue.isBusy([1, 2])).toBe(true);
    f.queue.setIds([1, 2]);
    expect(f.calls[1].ids).toEqual([2]);
    f.calls[1].resolve({ 2: 'image' });
    await flush();
    expect(f.queue.isBusy()).toBe(false);
  });

  it('backs off a failed batch while allowing a different viewport to load immediately', async () => {
    const f = fixture();
    f.queue.setIds([1, 2], { paused: false });
    const error = new Error('database busy');
    f.calls[0].reject(error);
    await flush();
    expect(f.onError).toHaveBeenCalledWith(error);
    f.queue.setIds([1, 2]);
    expect(f.calls).toHaveLength(1);
    f.queue.setIds([3, 4]);
    expect(f.calls[1].ids).toEqual([3, 4]);
    f.calls[1].resolve({ 3: 'image', 4: 'image' });
    await flush();
  });

  it('reloads evicted accepted images without bypassing the cooldown for missing images', async () => {
    vi.useFakeTimers();
    const f = fixture();
    f.queue.setIds([1, 2], { paused: false });
    f.calls[0].resolve({ 1: 'image' });
    await flush();
    f.cache.delete(1);
    f.queue.invalidate(1);
    f.queue.invalidate(2);
    f.queue.setIds([1, 2]);
    expect(f.calls[1].ids).toEqual([1]);
    f.calls[1].resolve({ 1: 'reloaded image' });
    await flush();
    expect(f.cache.has(1)).toBe(true);
    await vi.advanceTimersByTimeAsync(5000);
    f.queue.setIds([1, 2]);
    expect(f.calls[2].ids).toEqual([2]);
    f.calls[2].resolve({ 2: 'image' });
    await flush();
    expect(f.queue.isBusy()).toBe(false);
  });
});
