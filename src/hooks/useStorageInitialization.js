import { useCallback, useEffect, useRef, useState } from 'react';
import { getStorageInitializationStatus, retryStorageInitialization } from '../lib/storage_initialization_api';
import { formatError } from '../lib/errors';
import { usePolling } from './usePolling';
import { useTauriEventListener } from './useTauriEventListener';

const INITIAL_STATUS = { running: false, ready: false, phase: 'opening', processed: 0, total: 0, revision: -1, error: null };

export function useStorageInitialization() {
  const [status, setStatus] = useState(INITIAL_STATUS);
  const [statusKnown, setStatusKnown] = useState(false);
  const [readError, setReadError] = useState(null);
  const [retrying, setRetrying] = useState(false);
  const latest = useRef(INITIAL_STATUS);
  const alive = useRef(false);
  const retryPending = useRef(false);

  useEffect(() => {
    alive.current = true;
    return () => { alive.current = false; };
  }, []);

  const accept = useCallback((next) => {
    if (!alive.current) return;
    if (!next || typeof next.ready !== 'boolean') throw new Error('Invalid storage initialization status');
    // A pending poll or retry reply may land after a newer progress event.
    if (next.revision < latest.current.revision) return;
    latest.current = next;
    setStatus(next);
    setStatusKnown(true);
    setReadError(null);
  }, []);

  useTauriEventListener('storage-initialization-progress', ({ payload }) => accept(payload), [accept]);
  usePolling(async () => {
    try {
      const next = await getStorageInitializationStatus();
      accept(next);
      return !latest.current.ready;
    } catch (error) {
      if (alive.current && !latest.current.ready) setReadError(formatError(error));
      return true;
    }
  }, { intervalMs: 500, enabled: !status.ready });

  const retry = useCallback(async () => {
    if (retryPending.current) return;
    retryPending.current = true;
    setRetrying(true);
    try {
      accept(await retryStorageInitialization());
    } catch (error) {
      if (alive.current) setReadError(formatError(error));
    } finally {
      retryPending.current = false;
      if (alive.current) setRetrying(false);
    }
  }, [accept]);

  const error = status.error || readError;
  // A recreated WebView must query the existing job before showing progress.
  // Storage access stays gated by `ready` while that first read is pending.
  const showOverlay = !status.ready && (statusKnown || Boolean(error));
  return { status, ready: status.ready, showOverlay, error, retrying, retry };
}
