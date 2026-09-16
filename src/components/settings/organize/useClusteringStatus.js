import { useCallback, useEffect, useRef, useState } from 'react';
import { getClusteringStatus } from '../../../lib/task_api';

export function useClusteringStatus(monitorStatus, requestPending, active = true) {
  const [clusteringStatus, setClusteringStatus] = useState(null);
  const mounted = useRef(false);
  const generation = useRef(0);
  const inFlight = useRef(null);
  const currentMonitorStatus = useRef(monitorStatus);
  currentMonitorStatus.current = monitorStatus;

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      generation.current += 1;
    };
  }, []);

  const refreshClusteringStatus = useCallback(async ({ fresh = false } = {}) => {
    if (fresh && inFlight.current) {
      // A read started before the command finished may still describe its old
      // state. Wait for it, then read the result of the completed command.
      try { await inFlight.current; } catch { /* handled below */ }
    }
    if (!mounted.current || currentMonitorStatus.current !== 'running') return null;
    const currentGeneration = generation.current;
    const request = (!fresh && inFlight.current) || getClusteringStatus();
    inFlight.current = request;
    try {
      const result = await request;
      if (mounted.current && currentGeneration === generation.current && result?.status === 'success') {
        setClusteringStatus(result);
        return result;
      }
    } catch {
      // Do not keep presenting a stale run as active after losing access.
      if (mounted.current && currentGeneration === generation.current) {
        setClusteringStatus(null);
      }
    } finally {
      if (inFlight.current === request) inFlight.current = null;
    }
    return null;
  }, []);

  useEffect(() => {
    generation.current += 1;
    inFlight.current = null;
    if (monitorStatus !== 'running') setClusteringStatus(null);
  }, [monitorStatus]);

  useEffect(() => {
    if (!active || monitorStatus !== 'running') return;
    let cancelled = false;
    let timer;
    const poll = async () => {
      const status = await refreshClusteringStatus();
      if (cancelled) return;
      const task = status?.scheduler?.tasks?.find((item) => item.task_kind === 'python_clustering');
      const active = requestPending || status?.config?.running || task?.manual_pending;
      // Read immediately on entering the page or starting a request. Slow down
      // after completion while still discovering work started elsewhere.
      timer = setTimeout(poll, active ? 1500 : 10000);
    };
    poll();
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [monitorStatus, requestPending, refreshClusteringStatus, active]);

  return { clusteringStatus, refreshClusteringStatus };
}
