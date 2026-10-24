/** One small batch at a time; cancellation retains the slot until IPC settles. */
export function createTimelineBatchQueue(load, {
  batchSize,
  isLoaded,
  onResult,
  onError,
  onSettled,
  retryMs = 5000,
}) {
  let ids = [];
  let active = null;
  let paused = true;
  let generation = 0;
  const completed = new Set();
  const retryAt = new Map();
  const wanted = (desired = ids) => desired.filter((id) => !completed.has(id) && !isLoaded(id)
    && (retryAt.get(id) || 0) <= Date.now());

  const run = async () => {
    if (paused || active) return;
    const batch = wanted().slice(0, batchSize);
    if (batch.length === 0) return;
    const request = { ids: batch, controller: new AbortController(), generation };
    active = request;
    try {
      const result = await load(batch, { signal: request.controller.signal });
      if (request.generation === generation && !request.controller.signal.aborted) {
        const accepted = new Set(onResult(result, batch));
        for (const id of batch) {
          if (accepted.has(id)) {
            completed.add(id);
            retryAt.delete(id);
          }
          else retryAt.set(id, Date.now() + retryMs);
        }
      }
    } catch (error) {
      if (request.generation === generation && !request.controller.signal.aborted) {
        for (const id of batch) retryAt.set(id, Date.now() + retryMs);
        onError?.(error);
      }
    } finally {
      active = null;
      void run();
      onSettled?.();
    }
  };

  return {
    setIds(nextIds, { paused: nextPaused = paused } = {}) {
      // Apply priority and viewport together before admitting another batch.
      paused = nextPaused;
      ids = [...new Set(nextIds.filter((id) => Number.isInteger(id) && id > 0))];
      const desired = new Set(ids);
      for (const id of completed) if (!desired.has(id)) completed.delete(id);
      for (const id of retryAt.keys()) if (!desired.has(id)) retryAt.delete(id);
      if (active && (paused || !active.ids.some((id) => desired.has(id)))) active.controller.abort();
      void run();
    },
    setPaused(value) {
      paused = value;
      if (paused) active?.controller.abort();
      else void run();
    },
    isBusy(nextIds = ids) {
      return active !== null || wanted(nextIds).length > 0;
    },
    reset() {
      generation += 1;
      ids = [];
      completed.clear();
      retryAt.clear();
      active?.controller.abort();
    },
  };
}
