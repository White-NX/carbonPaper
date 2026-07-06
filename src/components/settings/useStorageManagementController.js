import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import {
  fetchThumbnailBatch,
  getIndexHealth,
  getProcessMonthlyThumbnails,
  getProcessStorageStats,
  getSoftDeleteQueueStatus,
  retryVectorIndexing,
  softDeleteProcessMonth,
  softDeleteScreenshots,
} from '../../lib/monitor_api';
import { withAuth } from '../../lib/auth_api';

export function useStorageManagementController({ storage, onRefresh, t, monitorStatus }) {
  const [storageLimit, setStorageLimit] = useState(() => {
    return localStorage.getItem('snapshotStorageLimit') || 'unlimited';
  });
  const [retentionPeriod, setRetentionPeriod] = useState(() => {
    return localStorage.getItem('snapshotRetentionPeriod') || 'permanent';
  });
  const [isMigrating, setIsMigrating] = useState(false);
  const [migrationProgress, setMigrationProgress] = useState({ total_files: 0, copied_files: 0, current_file: '' });
  const [migrationError, setMigrationError] = useState('');
  const [isUpdatingStoragePath, setIsUpdatingStoragePath] = useState(false);
  const [isMigrationChoiceDialogOpen, setIsMigrationChoiceDialogOpen] = useState(false);
  const [pendingTargetPath, setPendingTargetPath] = useState('');
  const [panelView, setPanelView] = useState('overview');
  const [processStats, setProcessStats] = useState([]);
  const [processStatsLoading, setProcessStatsLoading] = useState(false);
  const [processStatsError, setProcessStatsError] = useState('');
  const [selectedProcess, setSelectedProcess] = useState('');
  const [processPage, setProcessPage] = useState(0);
  const [processMonthData, setProcessMonthData] = useState(null);
  const [processMonthLoading, setProcessMonthLoading] = useState(false);
  const [processMonthError, setProcessMonthError] = useState('');
  const [processThumbMap, setProcessThumbMap] = useState({});
  const [selectedScreenshotIds, setSelectedScreenshotIds] = useState(() => new Set());
  const [deletingTarget, setDeletingTarget] = useState('');
  const [pendingDeleteIntent, setPendingDeleteIntent] = useState(null);
  const [isBackupDialogOpen, setIsBackupDialogOpen] = useState(false);
  const [backupMode, setBackupMode] = useState('export');
  const [deleteQueueStatus, setDeleteQueueStatus] = useState({
    pending_screenshots: 0,
    pending_ocr: 0,
    running: false,
  });
  const [indexHealth, setIndexHealth] = useState(null);
  const [indexHealthLoading, setIndexHealthLoading] = useState(false);
  const [indexHealthError, setIndexHealthError] = useState(null);
  const [vectorRetrying, setVectorRetrying] = useState(false);
  const mountedRef = useRef(true);
  const migrationUnlistenersRef = useRef([]);

  useEffect(() => {
    return () => {
      mountedRef.current = false;
      migrationUnlistenersRef.current.forEach((unlisten) => {
        try { unlisten(); } catch { }
      });
      migrationUnlistenersRef.current = [];
    };
  }, []);

  useEffect(() => {
    localStorage.setItem('snapshotStorageLimit', storageLimit);
    (async () => {
      try {
        await withAuth(() => invoke('storage_set_policy', { policy: { storage_limit: storageLimit, retention_period: retentionPeriod } }));
      } catch {
        // Backend may be unavailable in dev; localStorage remains the fallback.
      }
    })();
  }, [storageLimit]);

  useEffect(() => {
    localStorage.setItem('snapshotRetentionPeriod', retentionPeriod);
    (async () => {
      try {
        await withAuth(() => invoke('storage_set_policy', { policy: { storage_limit: storageLimit, retention_period: retentionPeriod } }));
      } catch {
        // Ignore backend sync failures here; the settings remain locally persisted.
      }
    })();
  }, [retentionPeriod]);

  useEffect(() => {
    (async () => {
      try {
        const resp = await withAuth(() => invoke('storage_get_policy'));
        if (resp && typeof resp === 'object') {
          if (resp.storage_limit) setStorageLimit(String(resp.storage_limit));
          if (resp.retention_period) setRetentionPeriod(String(resp.retention_period));
        }
      } catch {
        // Keep localStorage values when the backend is unavailable.
      }
    })();
  }, []);

  const loadDeleteQueueStatus = useCallback(async () => {
    const status = await getSoftDeleteQueueStatus();
    setDeleteQueueStatus(status || { pending_screenshots: 0, pending_ocr: 0, running: false });
  }, []);

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

  const loadProcessStats = useCallback(async () => {
    setProcessStatsLoading(true);
    setProcessStatsError('');
    try {
      const stats = await getProcessStorageStats();
      setProcessStats(Array.isArray(stats) ? stats : []);
    } catch (e) {
      setProcessStats([]);
      setProcessStatsError(String(e));
    } finally {
      setProcessStatsLoading(false);
    }
  }, []);

  const loadProcessMonthPage = useCallback(async (processName, page = 0) => {
    if (!processName) return;
    setProcessMonthLoading(true);
    setProcessMonthError('');
    try {
      const data = await getProcessMonthlyThumbnails(processName, page, 60);
      setProcessMonthData(data);
      setProcessPage(page);
      setSelectedScreenshotIds(new Set());
    } catch (e) {
      setProcessMonthData(null);
      setProcessMonthError(String(e));
    } finally {
      setProcessMonthLoading(false);
    }
  }, []);

  const openProcessDetail = useCallback((processName) => {
    setSelectedProcess(processName);
    setPanelView('process-detail');
    setProcessThumbMap({});
    setSelectedScreenshotIds(new Set());
    loadProcessMonthPage(processName, 0);
  }, [loadProcessMonthPage]);

  const toggleScreenshotSelection = useCallback((screenshotId) => {
    if (typeof screenshotId !== 'number' || screenshotId <= 0) return;
    setSelectedScreenshotIds((prev) => {
      const next = new Set(prev);
      if (next.has(screenshotId)) {
        next.delete(screenshotId);
      } else {
        next.add(screenshotId);
      }
      return next;
    });
  }, []);

  const requestSoftDelete = useCallback((processName, month = null, screenshotIds = []) => {
    if (!processName) return;
    const selectedIds = [...new Set((screenshotIds || []).filter((id) => typeof id === 'number' && id > 0))];
    const hasSelectedIds = selectedIds.length > 0;

    setPendingDeleteIntent({
      processName,
      month,
      screenshotIds: selectedIds,
      hasSelectedIds,
      targetKey: `${processName}::${month || 'all'}`,
      title: t('settings.storageManagement.deleteConfirm.title'),
      message: hasSelectedIds
        ? t('settings.storageManagement.deleteConfirm.messageSelected', { count: selectedIds.length })
        : month
          ? t('settings.storageManagement.deleteConfirm.messageMonth', { processName, month })
          : t('settings.storageManagement.deleteConfirm.messageProcess', { processName }),
      confirmLabel: hasSelectedIds
        ? t('settings.storageManagement.processDetails.deleteSelected', { count: selectedIds.length })
        : month
          ? t('settings.storageManagement.processDetails.deleteMonth')
          : t('settings.storageManagement.processDetails.deleteProcess'),
    });
  }, [t]);

  const executeSoftDelete = useCallback(async (intent) => {
    if (!intent?.processName) return;
    const {
      processName,
      month,
      screenshotIds,
      hasSelectedIds,
      targetKey,
    } = intent;

    setDeletingTarget(targetKey);
    try {
      if (hasSelectedIds) {
        await softDeleteScreenshots(screenshotIds);
        setSelectedScreenshotIds((prev) => {
          const next = new Set(prev);
          screenshotIds.forEach((id) => next.delete(id));
          return next;
        });
      } else {
        await softDeleteProcessMonth(processName, month);
      }
      await loadDeleteQueueStatus();
      await loadProcessStats();
      if (selectedProcess && selectedProcess === processName) {
        await loadProcessMonthPage(processName, processPage);
      }
      onRefresh?.();
    } catch (e) {
      setProcessMonthError(String(e));
    } finally {
      setDeletingTarget('');
    }
  }, [loadDeleteQueueStatus, loadProcessMonthPage, loadProcessStats, onRefresh, processPage, selectedProcess]);

  const handleConfirmSoftDelete = useCallback(async () => {
    if (!pendingDeleteIntent) return;
    await executeSoftDelete(pendingDeleteIntent);
    setPendingDeleteIntent(null);
  }, [executeSoftDelete, pendingDeleteIntent]);

  const handleCancelSoftDelete = useCallback(() => {
    if (deletingTarget) return;
    setPendingDeleteIntent(null);
  }, [deletingTarget]);

  useEffect(() => {
    loadDeleteQueueStatus();
    const timer = setInterval(loadDeleteQueueStatus, 5000);
    return () => clearInterval(timer);
  }, [loadDeleteQueueStatus]);

  useEffect(() => {
    loadIndexHealth({ refreshVector: false });
  }, [monitorStatus, loadIndexHealth]);

  useEffect(() => {
    if (panelView === 'overview') {
      loadProcessStats();
    }
  }, [panelView, loadProcessStats]);

  useEffect(() => {
    const items = processMonthData?.items || [];
    if (!items.length) {
      setProcessThumbMap({});
      return;
    }

    const ids = items.map((item) => item.screenshot_id).filter((id) => typeof id === 'number');
    fetchThumbnailBatch(ids)
      .then((batch) => setProcessThumbMap(batch || {}))
      .catch(() => setProcessThumbMap({}));
  }, [processMonthData]);

  const groupedMonthItems = useMemo(() => {
    const grouped = {};
    for (const item of processMonthData?.items || []) {
      const key = item.month || 'unknown';
      if (!grouped[key]) grouped[key] = [];
      grouped[key].push(item);
    }
    return Object.entries(grouped);
  }, [processMonthData]);

  const selectedCountByMonth = useMemo(() => {
    const counts = {};
    for (const item of processMonthData?.items || []) {
      if (!selectedScreenshotIds.has(item.screenshot_id)) continue;
      const month = item.month || 'unknown';
      counts[month] = (counts[month] || 0) + 1;
    }
    return counts;
  }, [processMonthData, selectedScreenshotIds]);

  const handleRefresh = useCallback(() => {
    onRefresh?.();
    loadDeleteQueueStatus();
    loadIndexHealth({ refreshVector: monitorStatus === 'running' });
    if (panelView === 'overview') {
      loadProcessStats();
    }
    if (panelView === 'process-detail' && selectedProcess) {
      loadProcessMonthPage(selectedProcess, processPage);
    }
  }, [loadDeleteQueueStatus, loadIndexHealth, loadProcessMonthPage, loadProcessStats, monitorStatus, onRefresh, panelView, processPage, selectedProcess]);

  const storageLimitOptions = [
    { value: '10', label: t('settings.storageManagement.storageLimit.options.10') },
    { value: '20', label: t('settings.storageManagement.storageLimit.options.20') },
    { value: '50', label: t('settings.storageManagement.storageLimit.options.50') },
    { value: '120', label: t('settings.storageManagement.storageLimit.options.120') },
    { value: 'unlimited', label: t('settings.storageManagement.storageLimit.options.unlimited') },
  ];

  const retentionOptions = [
    { value: '1month', label: t('settings.storageManagement.retention.options.1month') },
    { value: '6months', label: t('settings.storageManagement.retention.options.6months') },
    { value: '1year', label: t('settings.storageManagement.retention.options.1year') },
    { value: '2years', label: t('settings.storageManagement.retention.options.2years') },
    { value: 'permanent', label: t('settings.storageManagement.retention.options.permanent') },
  ];

  const diskInfo = useMemo(() => {
    const rootPath = storage?.root_path || '';
    const driveLetter = rootPath.charAt(0);

    return {
      driveLetter: driveLetter || 'C',
      totalSize: 500 * 1024 * 1024 * 1024,
      usedSize: 320 * 1024 * 1024 * 1024,
    };
  }, [storage]);

  const currentStoragePath = storage?.root_path || 'LocalAppData/CarbonPaper';

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

  const executeStoragePathChange = async (targetPath, shouldMigrateData) => {
    let unlistenProgress = null;
    let unlistenError = null;
    let shouldRestartMonitor = true;
    const registerMigrationListener = async (eventName, handler) => {
      const unlisten = await listen(eventName, (evt) => {
        if (mountedRef.current) {
          handler(evt);
        }
      });
      if (!mountedRef.current) {
        try { unlisten(); } catch { }
        return null;
      }
      migrationUnlistenersRef.current.push(unlisten);
      return unlisten;
    };
    const removeMigrationListener = async (unlisten) => {
      if (!unlisten) return;
      migrationUnlistenersRef.current = migrationUnlistenersRef.current.filter((fn) => fn !== unlisten);
      try { await unlisten(); } catch { }
    };

    try {
      if (!targetPath) return;

      setMigrationError('');
      setIsUpdatingStoragePath(true);
      try {
        const monitorStatusRaw = await invoke('get_monitor_status');
        const monitorStatus = typeof monitorStatusRaw === 'string' ? JSON.parse(monitorStatusRaw) : monitorStatusRaw;
        shouldRestartMonitor = !monitorStatus?.stopped;
      } catch {
        shouldRestartMonitor = true;
      }

      if (shouldRestartMonitor) {
        await withAuth(() => invoke('stop_monitor'), { autoPrompt: true });
      }

      if (shouldMigrateData) {
        setIsMigrating(true);
        setMigrationProgress({ total_files: 0, copied_files: 0, current_file: '' });

        unlistenProgress = await registerMigrationListener('storage-migration-progress', (evt) => {
          setMigrationProgress(evt.payload);
        });

        unlistenError = await registerMigrationListener('storage-migration-error', (evt) => {
          setMigrationError(evt.payload?.message || t('settings.storageManagement.migration.error_default'));
        });
      }

      await withAuth(() => invoke('storage_migrate_data_dir', {
        target: targetPath,
        migrateDataFiles: shouldMigrateData,
      }), { autoPrompt: true });

      if (shouldMigrateData) {
        if (mountedRef.current) {
          setMigrationProgress((s) => ({ ...s, current_file: t('settings.storageManagement.migration.completed') }));
        }
        await new Promise((resolve) => setTimeout(resolve, 600));
      }

      onRefresh?.();
    } catch (e) {
      console.error('change storage path failed', e);
      if (mountedRef.current) {
        setMigrationError(String(e));
      }
    } finally {
      await removeMigrationListener(unlistenProgress);
      await removeMigrationListener(unlistenError);

      if (mountedRef.current) {
        setIsMigrating(false);
        setIsUpdatingStoragePath(false);
      }
      if (shouldRestartMonitor) {
        try { await withAuth(() => invoke('start_monitor'), { autoPrompt: true }); } catch { }
      }
    }
  };

  const handleChangeStoragePath = async () => {
    try {
      const selected = await open({ directory: true });
      if (!selected) return;

      const targetPath = Array.isArray(selected) ? selected[0] : selected;
      if (!targetPath) return;

      const normalizedCurrent = currentStoragePath.replace(/[\\/]+$/, '');
      const normalizedTarget = targetPath.replace(/[\\/]+$/, '');
      if (normalizedCurrent && normalizedCurrent === normalizedTarget) {
        return;
      }

      setPendingTargetPath(targetPath);
      setIsMigrationChoiceDialogOpen(true);
    } catch (e) {
      console.error('select storage path failed', e);
      setMigrationError(String(e));
    }
  };

  const handleCancelMigrationChoice = () => {
    setIsMigrationChoiceDialogOpen(false);
    setPendingTargetPath('');
  };

  const handleApplyStoragePath = async (shouldMigrateData) => {
    const targetPath = pendingTargetPath;
    setIsMigrationChoiceDialogOpen(false);
    setPendingTargetPath('');
    await executeStoragePathChange(targetPath, shouldMigrateData);
  };

  return {
    storageLimit,
    setStorageLimit,
    retentionPeriod,
    setRetentionPeriod,
    isMigrating,
    migrationProgress,
    migrationError,
    isUpdatingStoragePath,
    isMigrationChoiceDialogOpen,
    pendingTargetPath,
    panelView,
    setPanelView,
    processStats,
    processStatsLoading,
    processStatsError,
    selectedProcess,
    processPage,
    processMonthData,
    processMonthLoading,
    processMonthError,
    processThumbMap,
    selectedScreenshotIds,
    deletingTarget,
    pendingDeleteIntent,
    isBackupDialogOpen,
    setIsBackupDialogOpen,
    backupMode,
    setBackupMode,
    deleteQueueStatus,
    indexHealth,
    indexHealthLoading,
    indexHealthError,
    vectorRetrying,
    groupedMonthItems,
    selectedCountByMonth,
    storageLimitOptions,
    retentionOptions,
    diskInfo,
    currentStoragePath,
    vectorRetryBacklog,
    indexHealthDeleteQueuePending,
    lastIndexingError,
    lastIndexingErrorAt,
    storageIpcLabel,
    storageIpcRetryAfter,
    handleRefresh,
    loadIndexHealth,
    handleRetryVectorIndexing,
    formatIndexCount,
    openProcessDetail,
    toggleScreenshotSelection,
    requestSoftDelete,
    handleConfirmSoftDelete,
    handleCancelSoftDelete,
    loadProcessMonthPage,
    handleChangeStoragePath,
    handleCancelMigrationChoice,
    handleApplyStoragePath,
  };
}
