import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { requestAuth, withAuth } from '../../../lib/auth_api';

export function useIndexTaskController(kind, progress, refresh, t) {
  const [submitting, setSubmitting] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [summary, setSummary] = useState(null);
  const [error, setError] = useState('');
  const submission = useRef(null);
  const stopPending = useRef(false);
  const key = `settings.advanced.${kind}_backend.`;

  useEffect(() => {
    if (!stopPending.current && !['running', 'queued', 'stopping'].includes(progress.phase)) {
      setStopping(false);
    }
  }, [progress]);

  const run = () => {
    if (submission.current || stopPending.current) return submission.current;
    setSubmitting(true);
    setSummary(null);
    setError('');
    const request = (async () => {
      try {
        // Verification wakes the existing request and preserves its progress.
        const result = ['waiting_for_unlock', 'waiting_for_verification'].includes(progress.phase)
          ? (await requestAuth() ? { queued: true } : null)
          : await withAuth(() => invoke(`${kind}_index_run_now`), { autoPrompt: true });
        setSummary(result);
        await refresh({ fresh: true });
        return result;
      } catch (cause) {
        console.warn(`Failed to start ${kind} indexing:`, cause);
        setError(t(key + 'run_start_failed'));
        return null;
      } finally {
        setSubmitting(false);
        submission.current = null;
      }
    })();
    submission.current = request;
    return request;
  };

  const stop = async () => {
    if (stopPending.current) return;
    stopPending.current = true;
    setStopping(true);
    setError('');
    try {
      // A quick stop must cancel the enqueue even when its reply is still pending.
      if (submission.current) await submission.current;
      await invoke(`${kind}_index_stop_now`);
      const next = await refresh({ fresh: true });
      if (next?.[kind] && !['running', 'queued', 'stopping'].includes(next[kind].phase)) {
        setStopping(false);
      }
    } catch (cause) {
      console.warn(`Failed to stop ${kind} indexing:`, cause);
      setError(t(key + 'run_stop_failed'));
      setStopping(false);
    } finally {
      stopPending.current = false;
    }
  };

  const phase = submitting ? 'queued' : progress.phase;
  return {
    run, stop, summary, error, phase,
    running: ['queued', 'running', 'stopping'].includes(phase),
    stopping: stopping || phase === 'stopping',
    retryAt: progress.retry_at_ms,
    progress: progress.total > 0 || progress.processed > 0 ? progress : null,
  };
}
