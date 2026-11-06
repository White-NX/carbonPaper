const aborted = () => new DOMException('Search request cancelled', 'AbortError');

/** Keep one running task and the newest pending task; identical work shares a promise. */
export function createLatestTaskQueue() {
  let active = null;
  let pending = null;

  const cancelEntry = (entry) => {
    if (!entry || entry.controller.signal.aborted) return;
    entry.controller.abort();
    entry.reject(aborted());
  };

  const start = (entry) => {
    active = entry;
    let work;
    try {
      work = entry.task(entry.controller.signal);
    } catch (error) {
      work = Promise.reject(error);
    }
    // Aborting an invoke does not release this slot until its actual work ends.
    // This prevents obsolete requests from accumulating in the native queue.
    Promise.resolve(work).then(entry.resolve, entry.reject).then(() => {
      active = null;
      const next = pending;
      pending = null;
      if (next && !next.controller.signal.aborted) start(next);
    });
  };

  return {
    run(key, task) {
      if (active?.key === key && !active.controller.signal.aborted) return active.promise;
      if (pending?.key === key) return pending.promise;
      cancelEntry(pending);
      const entry = { key, task, controller: new AbortController() };
      entry.promise = new Promise((resolve, reject) => { entry.resolve = resolve; entry.reject = reject; });
      if (active) {
        cancelEntry(active);
        pending = entry;
      } else {
        start(entry);
      }
      return entry.promise;
    },
    cancel() {
      cancelEntry(pending);
      pending = null;
      cancelEntry(active);
    },
  };
}

export function throwIfSearchAborted(signal) {
  if (signal?.aborted) throw aborted();
}
