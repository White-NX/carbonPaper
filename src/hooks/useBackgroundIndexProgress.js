import { useCallback, useEffect, useRef, useState } from 'react';
import { getBackgroundIndexProgress } from '../lib/monitor_api';
import { useTauriEventListener } from './useTauriEventListener';

const EMPTY_RUN = { phase: 'idle', running: false, processed: 0, indexed: 0, total: 0 };

function mergeRun(previous, incoming) {
  if (!incoming) return previous;
  if (incoming.revision != null && previous.revision != null && incoming.revision < previous.revision) {
    return previous;
  }
  return { ...incoming, phase: incoming.phase || (incoming.running ? 'running' : 'idle') };
}

export function isIndexTaskPending(run) {
  return ['queued', 'running', 'stopping', 'waiting_for_unlock', 'waiting_for_verification', 'retry_wait'].includes(run?.phase)
    || Boolean(run?.running);
}

export function useBackgroundIndexProgress(active = true) {
  const [progress, setProgress] = useState({ semantic: EMPTY_RUN, clip: EMPTY_RUN });
  const mounted = useRef(false);
  const inFlight = useRef(null);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  const refresh = useCallback(async ({ fresh = false } = {}) => {
    if (fresh && inFlight.current) await inFlight.current;
    if (inFlight.current) return inFlight.current;
    const request = (async () => {
      const next = await getBackgroundIndexProgress();
      if (mounted.current && next) {
        setProgress((previous) => ({
          semantic: mergeRun(previous.semantic, next.semantic),
          clip: mergeRun(previous.clip, next.clip),
        }));
      }
      return next;
    })();
    inFlight.current = request;
    try { return await request; }
    finally { if (inFlight.current === request) inFlight.current = null; }
  }, []);

  useEffect(() => {
    if (!active) return undefined;
    let cancelled = false;
    let timer;
    const poll = async () => {
      await refresh();
      if (!cancelled) timer = setTimeout(poll, 3000);
    };
    poll();
    return () => { cancelled = true; clearTimeout(timer); };
  }, [active, refresh]);

  const onChunk = (kind, payload) => {
    if (!payload) return;
    setProgress((previous) => ({
      ...previous,
      [kind]: mergeRun(previous[kind], {
        ...payload,
        running: true,
        phase: payload.stopping ? 'stopping' : 'running',
      }),
    }));
  };
  useTauriEventListener('semantic-index-progress', ({ payload }) => onChunk('semantic', payload), [], active);
  useTauriEventListener('clip-index-progress', ({ payload }) => onChunk('clip', payload), [], active);

  return { progress, refresh };
}
