import { useCallback, useEffect, useState } from 'react';
import { getIndexHealth, retryVectorIndexing } from '../../../lib/monitor_api';

export function useIndexHealthStatus({ monitorStatus, t }) {
  const [indexHealth, setIndexHealth] = useState(null);
  const [indexHealthLoading, setIndexHealthLoading] = useState(false);
  const [indexHealthError, setIndexHealthError] = useState(null);
  const [vectorRetrying, setVectorRetrying] = useState(false);

  const loadIndexHealth = useCallback(async ({ refreshVector = false } = {}) => {
    setIndexHealthLoading(true);
    setIndexHealthError(null);
    try {
      const result = await getIndexHealth({ refreshVector });
      setIndexHealth(result);
    } catch (err) {
      const message = err?.message || String(err);
      setIndexHealthError(message);
    } finally {
      setIndexHealthLoading(false);
    }
  }, []);

  const handleRetryVectorIndexing = useCallback(async () => {
    setVectorRetrying(true);
    setIndexHealthError(null);
    try {
      await retryVectorIndexing(32);
      await loadIndexHealth({ refreshVector: true });
    } catch (err) {
      const message = err?.message || String(err);
      setIndexHealthError(message);
    } finally {
      setVectorRetrying(false);
    }
  }, [loadIndexHealth]);

  const formatIndexCount = useCallback((value, fallback = '—') => {
    if (typeof value === 'number' && Number.isFinite(value)) {
      return value.toLocaleString();
    }
    return fallback;
  }, []);

  useEffect(() => {
    loadIndexHealth({ refreshVector: false });
  }, [monitorStatus, loadIndexHealth]);

  const vectorRetryBacklog = indexHealth?.pending_retry_queue_count;
  const indexHealthDeleteQueuePending = indexHealth
    ? (indexHealth.delete_queue?.pending_screenshots ?? 0) + (indexHealth.delete_queue?.pending_ocr ?? 0)
    : null;
  const lastIndexingError = indexHealth?.last_indexing_error;
  const lastIndexingErrorAt = indexHealth?.last_indexing_error_at
    ? new Date(indexHealth.last_indexing_error_at * 1000).toLocaleString()
    : null;
  const storageIpc = indexHealth?.storage_ipc || indexHealth?.python?.storage_ipc || null;
  const storageIpcState = storageIpc?.circuit_state;
  const storageIpcLabel = storageIpcState === 'open'
    ? t('settings.features.management.indexHealth.ipcOpen', '熔断')
    : storageIpcState === 'half_open'
      ? t('settings.features.management.indexHealth.ipcHalfOpen', '探测')
      : storageIpcState === 'closed'
        ? t('settings.features.management.indexHealth.ipcClosed', '正常')
        : '—';
  const storageIpcRetryAfter = typeof storageIpc?.retry_after_secs === 'number'
    ? Math.ceil(storageIpc.retry_after_secs)
    : null;

  return {
    indexHealth,
    indexHealthLoading,
    indexHealthError,
    vectorRetrying,
    vectorRetryBacklog,
    indexHealthDeleteQueuePending,
    lastIndexingError,
    lastIndexingErrorAt,
    storageIpcLabel,
    storageIpcRetryAfter,
    loadIndexHealth,
    handleRetryVectorIndexing,
    formatIndexCount,
  };
}
