import { useCallback, useEffect, useState } from 'react';
import { getIndexHealth } from '../../../lib/monitor_api';

export function useIndexHealthStatus() {
  const [indexHealth, setIndexHealth] = useState(null);
  const [indexHealthLoading, setIndexHealthLoading] = useState(false);
  const [indexHealthError, setIndexHealthError] = useState(null);

  const loadIndexHealth = useCallback(async () => {
    setIndexHealthLoading(true);
    setIndexHealthError(null);
    try {
      const result = await getIndexHealth();
      setIndexHealth(result);
    } catch (err) {
      const message = err?.message || String(err);
      setIndexHealthError(message);
    } finally {
      setIndexHealthLoading(false);
    }
  }, []);

  const formatIndexCount = useCallback((value, fallback = '—') => {
    if (typeof value === 'number' && Number.isFinite(value)) {
      return value.toLocaleString();
    }
    return fallback;
  }, []);

  useEffect(() => {
    loadIndexHealth();
  }, [loadIndexHealth]);

  const indexBacklog = (indexHealth?.semantic_index_backlog?.claimable ?? 0)
    + (indexHealth?.clip_index_backlog?.claimable ?? 0);
  const indexHealthDeleteQueuePending = indexHealth
    ? (indexHealth.delete_queue?.pending_screenshots ?? 0) + (indexHealth.delete_queue?.pending_ocr ?? 0)
    : null;
  return {
    indexHealth,
    indexHealthLoading,
    indexHealthError,
    indexBacklog,
    indexHealthDeleteQueuePending,
    loadIndexHealth,
    formatIndexCount,
  };
}
