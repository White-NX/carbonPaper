import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getRecapDay } from '../lib/recap_api';
import useRecapProgress from './useRecapProgress';

/** Read saved outcomes on cache changes, not on every streamed text delta. */
export default function useRecapDiagnostics(date, active) {
  const { progress, error: progressError } = useRecapProgress(date, active, active);
  const [state, setState] = useState({ date, day: null, error: '' });
  const refreshRef = useRef(() => {});
  useEffect(() => {
    setState({ date, day: null, error: '' });
    if (!active) return undefined;
    let live = true;
    let loading = false;
    let pending = false;
    const refresh = async () => {
      if (!live || document.hidden) return;
      if (loading) { pending = true; return; }
      loading = true;
      try {
        const day = await getRecapDay(date, { includeRecords: false, includeAttempts: true });
        if (live) setState({ date, day, error: '' });
      } catch (error) {
        if (live) setState({ date, day: null, error: String(error) });
      } finally {
        loading = false;
        if (pending && live) { pending = false; refresh(); }
      }
    };
    refreshRef.current = refresh;
    const subscription = listen('recap-changed', refresh);
    subscription.then(refresh).catch(() => { if (live) refresh(); });
    window.addEventListener('focus', refresh);
    document.addEventListener('visibilitychange', refresh);
    return () => {
      live = false;
      refreshRef.current = () => {};
      window.removeEventListener('focus', refresh);
      document.removeEventListener('visibilitychange', refresh);
      subscription.then(unlisten => unlisten()).catch(() => {});
    };
  }, [date, active]);
  // Recover a missed cache notification at run checkpoints, including stop/pause.
  useEffect(() => {
    if (progress?.run_id != null) refreshRef.current();
  }, [progress?.run_id, progress?.completed_batches, progress?.finished_at_ms]);
  return {
    day: active && state.date === date && !progressError ? state.day : null,
    error: active && state.date === date ? state.error || progressError : '',
    progress: state.error || progressError ? null : progress,
  };
}
