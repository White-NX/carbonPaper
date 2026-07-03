import { afterEach, describe, expect, it, vi } from 'vitest';
import { RequestQueue } from './monitor_api';

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

describe('RequestQueue', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('limits concurrent task execution', async () => {
    const queue = new RequestQueue(2, 20);
    let running = 0;
    let maxRunning = 0;

    const task = async () => {
      running += 1;
      maxRunning = Math.max(maxRunning, running);
      await sleep(20);
      running -= 1;
      return 'ok';
    };

    await Promise.all([
      queue.enqueue(task),
      queue.enqueue(task),
      queue.enqueue(task),
      queue.enqueue(task),
      queue.enqueue(task),
    ]);

    expect(maxRunning).toBeLessThanOrEqual(2);
  });

  it('dedupes pending requests by key', async () => {
    const queue = new RequestQueue(1, 10);
    const fn = vi.fn(async () => {
      await sleep(10);
      return 'done';
    });

    const p1 = queue.enqueue(() => fn(), { key: 'same-key' });
    const p2 = queue.enqueue(() => fn(), { key: 'same-key' });

    await expect(p1).resolves.toBe('done');
    await expect(p2).resolves.toBe('done');
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it('clears and cancels pending tasks', async () => {
    const queue = new RequestQueue(1, 10);
    let releaseFirst;

    const first = queue.enqueue(
      () =>
        new Promise((resolve) => {
          releaseFirst = resolve;
        })
    );

    const second = queue.enqueue(() => Promise.resolve('second'));
    const third = queue.enqueue(() => Promise.resolve('third'));

    queue.clearPending();

    await expect(second).rejects.toThrow('cancelled');
    await expect(third).rejects.toThrow('cancelled');

    releaseFirst('first');
    await expect(first).resolves.toBe('first');
  });

  it('rejects pending tasks that exceed their deadline before running', async () => {
    vi.useFakeTimers();
    const queue = new RequestQueue(1, 10);
    let releaseFirst;
    const secondFn = vi.fn(() => Promise.resolve('second'));

    const first = queue.enqueue(
      () =>
        new Promise((resolve) => {
          releaseFirst = resolve;
        }),
      { deadlineMs: 1000 }
    );
    const second = queue.enqueue(secondFn, { deadlineMs: 50 });
    const secondAssertion = expect(second).rejects.toMatchObject({ code: 'deadline_exceeded' });

    await vi.advanceTimersByTimeAsync(51);
    await secondAssertion;
    expect(secondFn).not.toHaveBeenCalled();

    releaseFirst('first');
    await expect(first).resolves.toBe('first');
  });

  it('releases a running queue slot when a task exceeds its deadline', async () => {
    vi.useFakeTimers();
    const queue = new RequestQueue(1, 10);

    const hung = queue.enqueue(() => new Promise(() => {}), { deadlineMs: 50 });
    const hungAssertion = expect(hung).rejects.toMatchObject({ code: 'deadline_exceeded' });
    const second = queue.enqueue(() => Promise.resolve('second'), { deadlineMs: 500 });

    await vi.advanceTimersByTimeAsync(51);

    await hungAssertion;
    await expect(second).resolves.toBe('second');
  });
});
