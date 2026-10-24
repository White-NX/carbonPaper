/** One active read and one latest pending viewport. An abort does not free the
 * slot until the backend acknowledges it by settling the read promise. */
export function createTimelineRequestQueue(load, { onResult, onError, onBusyChange }) {
  let active = null;
  let pending = null;
  let busy = false;
  const sameRange = (a, b) => a.start === b.start && a.end === b.end && a.sampleMs === b.sampleMs;
  const setBusy = (value) => {
    if (busy === value) return;
    busy = value;
    onBusyChange?.(value);
  };

  const run = async (range) => {
    const request = { range, controller: new AbortController() };
    active = request;
    setBusy(true);
    try {
      const records = await load(range, request.controller.signal);
      if (!request.controller.signal.aborted) onResult(records, range);
    } catch (error) {
      if (!request.controller.signal.aborted) onError(error, range);
    } finally {
      active = null;
      const next = pending;
      pending = null;
      if (next) void run(next);
      else setBusy(false);
    }
  };

  return {
    request(range, { refresh = false } = {}) {
      if (!active) {
        void run(range);
        return;
      }
      if (!active.controller.signal.aborted && sameRange(active.range, range)) {
        pending = null;
        return;
      }
      pending = range;
      // Periodic follow refreshes must not starve a slow but useful read.
      // Gestures supersede the active viewport immediately.
      if (!refresh) active.controller.abort();
    },
    cancel() {
      pending = null;
      active?.controller.abort();
    },
  };
}
