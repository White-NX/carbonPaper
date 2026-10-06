import { describe, expect, it, vi } from 'vitest';
import { createTimelineRequestQueue } from './timeline_requests';

const range = (start) => ({ start, end: start + 100, sampleMs: 1 });
const flush = async () => { await Promise.resolve(); await Promise.resolve(); };
function fixture() {
  const calls = [];
  const load = vi.fn((viewport, signal) => new Promise((resolve, reject) => {
    calls.push({ viewport, signal, resolve, reject });
  }));
  const onResult = vi.fn();
  const onError = vi.fn();
  return { calls, load, onResult, onError, queue: createTimelineRequestQueue(load, { onResult, onError }) };
}

describe('timeline request scheduling', () => {
  it('waits for backend cancellation and retains only the latest of many viewports', async () => {
    const f = fixture();
    f.queue.request(range(0));
    for (let i = 1; i <= 30; i += 1) f.queue.request(range(i));
    expect(f.calls).toHaveLength(1);
    expect(f.calls[0].signal.aborted).toBe(true);
    f.calls[0].resolve(['stale']);
    await flush();
    expect(f.onResult).not.toHaveBeenCalled();
    expect(f.calls).toHaveLength(2);
    expect(f.calls[1].viewport).toEqual(range(30));
    f.calls[1].resolve(['latest']);
    await flush();
    expect(f.onResult).toHaveBeenCalledWith(['latest'], range(30));
  });

  it('coalesces repeated refreshes without aborting or starving a useful result', async () => {
    const f = fixture();
    f.queue.request(range(0));
    f.queue.request(range(0), { refresh: true });
    f.queue.request(range(1), { refresh: true });
    f.queue.request(range(2), { refresh: true });
    expect(f.calls[0].signal.aborted).toBe(false);
    f.calls[0].resolve(['useful']);
    await flush();
    expect(f.onResult).toHaveBeenCalledWith(['useful'], range(0));
    expect(f.calls[1].viewport).toEqual(range(2));
    f.calls[1].resolve([]);
    await flush();
  });

  it('does not queue duplicate reads for an unchanged range', async () => {
    const f = fixture();
    f.queue.request(range(0));
    for (let i = 0; i < 10; i += 1) f.queue.request(range(0), { refresh: true });
    f.calls[0].resolve([]);
    await flush();
    expect(f.calls).toHaveLength(1);
  });

  it('cancels on disposal and can resume while the cancelled read is settling', async () => {
    const f = fixture();
    f.queue.request(range(0));
    f.queue.request(range(1));
    f.queue.cancel();
    f.queue.request(range(2));
    expect(f.calls).toHaveLength(1);
    f.calls[0].reject(new Error('cancelled'));
    await flush();
    expect(f.onError).not.toHaveBeenCalled();
    expect(f.calls[1].viewport).toEqual(range(2));
    f.queue.cancel();
    f.calls[1].resolve([]);
    await flush();
    expect(f.onResult).not.toHaveBeenCalled();
    expect(f.calls).toHaveLength(2);
  });

  it('recovers from a failed read and still runs the latest refresh', async () => {
    const f = fixture();
    f.queue.request(range(0));
    f.queue.request(range(1), { refresh: true });
    const error = new Error('query failed');
    f.calls[0].reject(error);
    await flush();
    expect(f.onError).toHaveBeenCalledWith(error, range(0));
    expect(f.calls[1].viewport).toEqual(range(1));
    f.calls[1].resolve([]);
    await flush();
  });
});
