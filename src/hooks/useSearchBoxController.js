import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import {
  searchScreenshots,
  fetchThumbnailBatch,
  getSoftDeleteQueueStatus,
  getSmartClusterWorkerStatus,
  normalizeSmartClusterWorkerStatus,
  getBackgroundIndexProgress,
} from '../lib/monitor_api';
import { smartClusterStopDrain } from '../lib/task_api';
import { useHmacMigrationStatus } from './useHmacMigrationStatus';

function useDebounce(value, delay) {
  const [debouncedValue, setDebouncedValue] = useState(value);

  useEffect(() => {
    const handler = setTimeout(() => {
      setDebouncedValue(value);
    }, delay);
    return () => {
      clearTimeout(handler);
    };
  }, [value, delay]);

  return debouncedValue;
}

const EMPTY_DELETE_QUEUE_STATUS = {
  pending_screenshots: 0,
  pending_ocr: 0,
  running: false,
};

const EMPTY_CLUSTER_QUEUE_STATUS = {
  pending_count: 0,
  running: false,
  forceRunning: false,
  manualActive: false,
  phase: 'idle',
  total: 0,
  processed: 0,
};

// A manual index run that has not been observed yet. `total` of zero means "the
// run has not finished counting its queue", not "there is nothing to do", which
// is why the percentage below treats it as indeterminate.
const EMPTY_INDEX_RUN = {
  running: false,
  processed: 0,
  indexed: 0,
  total: 0,
};

const EMPTY_BACKGROUND_INDEX_PROGRESS = {
  semantic: EMPTY_INDEX_RUN,
  clip: EMPTY_INDEX_RUN,
};

/**
 * Percentage of one manual index run, or `null` while it is indeterminate.
 *
 * The backend reports the queue depth it started with, so this is a real ratio
 * rather than the peak-tracking estimate the delete and cluster queues need —
 * neither of those knows its own total. Clamped because a run that started while
 * captures were still arriving can process more than it counted.
 */
function indexRunPercent(run) {
  if (!run?.running || !run.total) return null;
  return Math.max(0, Math.min(100, (run.processed / run.total) * 100));
}

export function useSearchBoxController({
  onSelectResult,
  onSubmit,
  controlledMode,
  onModeChange,
  monitorPaused,
  handlePauseMonitor,
  handleResumeMonitor,
  t,
}) {
  const [query, setQuery] = useState('');
  const [localMode, setLocalMode] = useState('ocr');
  const [showModeMenu, setShowModeMenu] = useState(false);
  const [results, setResults] = useState([]);
  const [error, setError] = useState(null);
  const [loading, setLoading] = useState(false);
  const [showResults, setShowResults] = useState(false);
  const [thumbCache, setThumbCache] = useState({});
  const [deleteQueueStatus, setDeleteQueueStatus] = useState(EMPTY_DELETE_QUEUE_STATUS);
  const [deleteQueuePeak, setDeleteQueuePeak] = useState(0);
  const [smartClusterQueueStatus, setSmartClusterQueueStatus] = useState(EMPTY_CLUSTER_QUEUE_STATUS);
  const [smartClusterQueuePeak, setSmartClusterQueuePeak] = useState(0);
  const [downloadProgress, setDownloadProgress] = useState(0);
  const [isDownloadingModels, setIsDownloadingModels] = useState(false);
  const [backgroundIndex, setBackgroundIndex] = useState(EMPTY_BACKGROUND_INDEX_PROGRESS);

  const debouncedQuery = useDebounce(query, 500);
  const wrapperRef = useRef(null);
  const inputRef = useRef(null);
  const userInteractionRef = useRef(false);
  const wasPausedRef = useRef(null);
  const handleResumeMonitorRef = useRef(handleResumeMonitor);
  const mode = controlledMode ?? localMode;
  const setMode = onModeChange ?? setLocalMode;
  const isMigrating = useHmacMigrationStatus();

  useEffect(() => {
    function handleClickOutside(event) {
      if (wrapperRef.current && !wrapperRef.current.contains(event.target)) {
        setShowModeMenu(false);
        setShowResults(false);
      }
    }
    document.addEventListener('mousedown', handleClickOutside);
    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
    };
  }, []);

  useEffect(() => {
    if (debouncedQuery.trim().length === 0) {
      setResults([]);
      setError(null);
      setLoading(false);
      setShowResults(false);
      return;
    }

    let active = true;
    const doSearch = async () => {
      setLoading(true);
      setError(null);
      try {
        const res = await searchScreenshots(debouncedQuery, mode);
        if (!active) return;
        setResults(res);
        const isFocused = document.activeElement === inputRef.current;
        if (isFocused || userInteractionRef.current) {
          setShowResults(true);
        }
        userInteractionRef.current = false;
      } catch (e) {
        if (!active) return;
        console.error('Search failed:', e);
        setError(e.message || String(e));
        setResults([]);
        setShowResults(true);
      } finally {
        if (active) setLoading(false);
      }
    };

    doSearch();
    return () => {
      active = false;
    };
  }, [debouncedQuery, mode]);

  useEffect(() => {
    if (!results.length) {
      setThumbCache({});
      return undefined;
    }
    let active = true;
    const ids = results
      .map((item) => {
        const sid = mode === 'nl' ? item.metadata?.screenshot_id : item.screenshot_id;
        return typeof sid === 'number' && sid > 0 ? sid : null;
      })
      .filter(Boolean);

    if (ids.length === 0) return undefined;
    fetchThumbnailBatch([...new Set(ids)])
      .then((batch) => {
        if (active && batch) setThumbCache(batch);
      })
      .catch(() => {});

    return () => {
      active = false;
    };
  }, [results, mode]);

  useEffect(() => {
    const handleProgress = (event) => {
      if (event.detail) {
        setDownloadProgress(event.detail.progress ?? 0);
        setIsDownloadingModels(event.detail.active ?? false);
      }
    };
    window.addEventListener('model-download-progress', handleProgress);
    return () => {
      window.removeEventListener('model-download-progress', handleProgress);
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    const loadQueueStatus = async () => {
      try {
        const status = await getSoftDeleteQueueStatus();
        if (!cancelled) {
          setDeleteQueueStatus(status || EMPTY_DELETE_QUEUE_STATUS);
        }
      } catch {
        if (!cancelled) {
          setDeleteQueueStatus(EMPTY_DELETE_QUEUE_STATUS);
        }
      }
    };

    loadQueueStatus();
    const timer = setInterval(loadQueueStatus, 4000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    const loadClusterStatus = async () => {
      try {
        const config = await invoke('get_advanced_config');
        if (cancelled) return;
        if (config && config.smart_cluster_enabled) {
          const status = await getSmartClusterWorkerStatus();
          if (!cancelled) {
            setSmartClusterQueueStatus(status || EMPTY_CLUSTER_QUEUE_STATUS);
          }
        } else {
          setSmartClusterQueueStatus(EMPTY_CLUSTER_QUEUE_STATUS);
        }
      } catch {
        if (!cancelled) {
          setSmartClusterQueueStatus(EMPTY_CLUSTER_QUEUE_STATUS);
        }
      }
    };

    loadClusterStatus();
    const timer = setInterval(loadClusterStatus, 4000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    const loadBackgroundIndex = async () => {
      const progress = await getBackgroundIndexProgress();
      if (!cancelled) setBackgroundIndex(progress);
    };

    loadBackgroundIndex();
    const timer = setInterval(loadBackgroundIndex, 4000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  useEffect(() => {
    let unlisten = null;
    let cancelled = false;
    listen('smart-cluster-progress', ({ payload }) => {
      if (!payload || cancelled) return;
      const normalized = typeof normalizeSmartClusterWorkerStatus === 'function'
        ? normalizeSmartClusterWorkerStatus(payload)
        : payload;
      setSmartClusterQueueStatus((previous) => ({
        ...previous,
        ...normalized,
      }));
    }).then((dispose) => {
      if (cancelled) dispose();
      else unlisten = dispose;
    }).catch((err) => console.warn('Failed to subscribe to smart-cluster-progress:', err));
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  // Index runs already publish chunk events. Keep the existing event-driven
  // smoothing for those queues; Smart Cluster has its own queue event above.
  useEffect(() => {
    const subscriptions = [
      ['semantic-index-progress', 'semantic'],
      ['clip-index-progress', 'clip'],
    ].map(([event, key]) => {
      let unlisten = null;
      let cancelled = false;
      listen(event, ({ payload }) => {
        if (!payload) return;
        setBackgroundIndex((prev) => (prev[key].running
          ? { ...prev, [key]: { ...prev[key], ...payload } }
          : prev));
      }).then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      }).catch((err) => {
        console.warn(`Failed to subscribe to ${event}:`, err);
      });
      return () => {
        cancelled = true;
        if (unlisten) unlisten();
      };
    });
    return () => subscriptions.forEach((dispose) => dispose());
  }, []);

  const pendingDeleteTotal = Number(deleteQueueStatus?.pending_ocr || 0)
    + Number(deleteQueueStatus?.pending_screenshots || 0);
  const hasDeleteTask = Boolean(deleteQueueStatus?.running) || pendingDeleteTotal > 120;
  const deleteProgress = (() => {
    if (!hasDeleteTask) return 0;
    if (pendingDeleteTotal <= 0) return 100;
    if (deleteQueuePeak <= 0) return 0;
    const ratio = ((deleteQueuePeak - pendingDeleteTotal) / deleteQueuePeak) * 100;
    return Math.max(0, Math.min(100, ratio));
  })();

  const clusterManualActive = Boolean(smartClusterQueueStatus?.manualActive)
    || Boolean(smartClusterQueueStatus?.forceRunning);
  const clusterActive = clusterManualActive || Boolean(smartClusterQueueStatus?.running);
  const hasClusterTask = clusterActive
    && (Number(smartClusterQueueStatus?.pending_count || 0) > 0
      || ['queued', 'running', 'waiting', 'stopping', 'retrying']
        .includes(smartClusterQueueStatus?.phase));
  const canCancelClusterTask = hasClusterTask && clusterManualActive;
  const clusterProgress = (() => {
    if (!hasClusterTask) return 0;
    const pending = Number(smartClusterQueueStatus.pending_count || 0);
    const total = Number(smartClusterQueueStatus.total || smartClusterQueuePeak || 0);
    if (pending <= 0) return 100;
    if (total <= 0) return 0;
    const processed = Number(smartClusterQueueStatus.processed || 0);
    const ratio = (processed / total) * 100;
    return Math.max(0, Math.min(100, ratio));
  })();

  const hasSemanticIndexTask = backgroundIndex.semantic.running;
  const semanticIndexPercent = indexRunPercent(backgroundIndex.semantic);

  const hasClipIndexTask = backgroundIndex.clip.running;
  const clipIndexPercent = indexRunPercent(backgroundIndex.clip);

  const showProgressBar = hasDeleteTask || hasClusterTask || isDownloadingModels
    || hasSemanticIndexTask || hasClipIndexTask;
  const progressFillPercent = (() => {
    if (hasDeleteTask) return deleteProgress <= 0 ? 8 : Math.min(100, deleteProgress);
    if (hasClusterTask) return clusterProgress <= 0 ? 8 : Math.min(100, clusterProgress);
    if (isDownloadingModels) return downloadProgress <= 0 ? 8 : Math.min(100, downloadProgress);
    // The index runs come last so that this stays a pure addition to a chain
    // that already worked. A backfill can run for hours, and letting it preempt
    // the cleanup indicator for that long would trade one silent task for
    // another. The dropdown cards are not ordered, so the reason a backfill
    // stalls the rest of indexing stays visible either way.
    if (hasClipIndexTask) {
      return clipIndexPercent === null || clipIndexPercent <= 0
        ? 8
        : Math.min(100, clipIndexPercent);
    }
    if (hasSemanticIndexTask) {
      return semanticIndexPercent === null || semanticIndexPercent <= 0
        ? 8
        : Math.min(100, semanticIndexPercent);
    }
    return 0;
  })();

  const taskSummaryPlaceholder = (() => {
    if (hasDeleteTask) {
      return t('search.task.summaryPlaceholder', { progress: Math.round(deleteProgress) });
    }
    if (hasClusterTask) {
      return t('search.task.smartClusterSummaryPlaceholder', { progress: Math.round(clusterProgress) });
    }
    if (isDownloadingModels) {
      return t('search.task.modelDownloadSummaryPlaceholder', { progress: Math.round(downloadProgress) });
    }
    if (hasClipIndexTask) {
      const progress = clipIndexPercent === null ? '…' : Math.round(clipIndexPercent);
      return t('search.task.clipIndexSummaryPlaceholder', { progress });
    }
    if (hasSemanticIndexTask) {
      const progress = semanticIndexPercent === null ? '…' : Math.round(semanticIndexPercent);
      return t('search.task.semanticIndexSummaryPlaceholder', { progress });
    }
    return '';
  })();

  useEffect(() => {
    if (!hasDeleteTask) {
      setDeleteQueuePeak(0);
      return;
    }
    if (pendingDeleteTotal > 0) {
      setDeleteQueuePeak((prev) => Math.max(prev, pendingDeleteTotal));
    }
  }, [hasDeleteTask, pendingDeleteTotal]);

  useEffect(() => {
    const running = clusterActive;
    const pending = Number(smartClusterQueueStatus?.pending_count || 0);
    if (!running || pending <= 0) {
      setSmartClusterQueuePeak(0);
      return;
    }
    setSmartClusterQueuePeak((prev) => Math.max(prev, pending));
  }, [clusterActive, smartClusterQueueStatus?.pending_count]);

  useEffect(() => {
    handleResumeMonitorRef.current = handleResumeMonitor;
  }, [handleResumeMonitor]);

  useEffect(() => {
    if (hasClusterTask) {
      if (wasPausedRef.current === null) {
        wasPausedRef.current = !!monitorPaused;
        if (!monitorPaused && handlePauseMonitor) {
          handlePauseMonitor();
        }
      }
    } else if (wasPausedRef.current !== null) {
      const wasPaused = wasPausedRef.current;
      wasPausedRef.current = null;
      if (!wasPaused && handleResumeMonitor) {
        handleResumeMonitor();
      }
    }
  }, [hasClusterTask, monitorPaused, handlePauseMonitor, handleResumeMonitor]);

  useEffect(() => {
    return () => {
      if (wasPausedRef.current === false && handleResumeMonitorRef.current) {
        try {
          handleResumeMonitorRef.current();
        } catch (e) {
          console.warn('[SmartCluster] resume on unmount failed', e);
        }
      }
      wasPausedRef.current = null;
    };
  }, []);

  const switchMode = (nextMode) => {
    userInteractionRef.current = true;
    setMode(nextMode);
  };

  const toggleMode = () => {
    switchMode(mode === 'ocr' ? 'nl' : 'ocr');
  };

  const selectMode = (nextMode) => {
    switchMode(nextMode);
    setShowModeMenu(false);
  };

  const handleSelect = (item) => {
    const id = mode === 'ocr' ? item.screenshot_id : item.metadata?.screenshot_id;
    onSelectResult({
      id,
      ...item,
      path: item.image_path || item.path,
    });
    setShowResults(false);
  };

  const handleViewMore = (event) => {
    event?.preventDefault?.();
    setShowResults(false);
    onSubmit?.({ query, mode });
  };

  const handleSubmit = (event) => {
    if (event.key !== 'Enter') return;
    event.preventDefault();
    handleViewMore(event);
  };

  const handleCancelCluster = async (event) => {
    event?.stopPropagation();
    try {
      await smartClusterStopDrain();
      const status = await getSmartClusterWorkerStatus();
      setSmartClusterQueueStatus(status || EMPTY_CLUSTER_QUEUE_STATUS);
    } catch (err) {
      console.error('Failed to cancel cluster process:', err);
    }
  };

  return {
    query,
    setQuery,
    mode,
    showModeMenu,
    setShowModeMenu,
    results,
    error,
    loading,
    showResults,
    setShowResults,
    debouncedQuery,
    wrapperRef,
    inputRef,
    thumbCache,
    isMigrating,
    deleteQueueStatus,
    smartClusterQueueStatus,
    downloadProgress,
    hasDeleteTask,
    deleteProgress,
    hasClusterTask,
    canCancelClusterTask,
    clusterProgress,
    hasSemanticIndexTask,
    semanticIndexPercent,
    hasClipIndexTask,
    clipIndexPercent,
    showProgressBar,
    progressFillPercent,
    taskSummaryPlaceholder,
    isDownloadingModels,
    toggleMode,
    selectMode,
    handleSelect,
    handleSubmit,
    handleViewMore,
    handleCancelCluster,
  };
}
