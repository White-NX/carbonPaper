import { describe, expect, it, vi } from 'vitest';
import { createLatestTaskQueue } from './latest_task_queue';

const deferred = () => {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
};

describe('latest search tasks', () => {
  it('shares identical running requests', async () => {
    const queue = createLatestTaskQueue();
    const work = deferred();
    const first = queue.run('same', () => work.promise);
    const unused = vi.fn();
    expect(queue.run('same', unused)).toBe(first);
    work.resolve('result');
    await expect(first).resolves.toBe('result');
    expect(unused).not.toHaveBeenCalled();
  });

  it('runs only the newest pending query after actual native work finishes', async () => {
    const queue = createLatestTaskQueue();
    const work = deferred();
    let runningSignal;
    const first = queue.run('old', (signal) => { runningSignal = signal; return work.promise; }).catch((error) => error);
    const middleTask = vi.fn();
    const middle = queue.run('middle', middleTask).catch((error) => error);
    const newestTask = vi.fn(async () => 'newest result');
    const newest = queue.run('newest', newestTask);
    expect(runningSignal.aborted).toBe(true);
    expect(newestTask).not.toHaveBeenCalled();
    work.resolve('obsolete');
    await expect(newest).resolves.toBe('newest result');
    expect((await first).name).toBe('AbortError');
    expect((await middle).name).toBe('AbortError');
    expect(middleTask).not.toHaveBeenCalled();
    expect(newestTask).toHaveBeenCalledOnce();
  });

  it('cancels queued work at a session boundary without releasing an active invoke', async () => {
    const queue = createLatestTaskQueue();
    const work = deferred();
    const first = queue.run('active', () => work.promise).catch((error) => error);
    const discarded = vi.fn();
    const pending = queue.run('discarded', discarded).catch((error) => error);
    queue.cancel();
    const resumedTask = vi.fn(async () => 'resumed');
    const resumed = queue.run('resumed', resumedTask);
    expect(resumedTask).not.toHaveBeenCalled();
    work.resolve('old');
    await expect(resumed).resolves.toBe('resumed');
    expect((await first).name).toBe('AbortError');
    expect((await pending).name).toBe('AbortError');
    expect(discarded).not.toHaveBeenCalled();
  });

  it('continues after a failed task', async () => {
    const queue = createLatestTaskQueue();
    await expect(queue.run('failed', () => { throw new Error('failed'); })).rejects.toThrow('failed');
    await expect(queue.run('next', async () => 'ready')).resolves.toBe('ready');
  });
});
