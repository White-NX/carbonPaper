import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  fetchThumbnailBatch,
  getProcessMonthlyThumbnails,
  getProcessStorageStats,
  getSoftDeleteQueueStatus,
  softDeleteProcessMonth,
  softDeleteScreenshots,
} from '../../../lib/monitor_api';

export function useProcessStorageDetails({ onRefresh, t }) {
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

  const loadDeleteQueueStatus = useCallback(async () => {
    const status = await getSoftDeleteQueueStatus();
    setDeleteQueueStatus(status || { pending_screenshots: 0, pending_ocr: 0, running: false });
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

  return {
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
    groupedMonthItems,
    selectedCountByMonth,
    loadDeleteQueueStatus,
    loadProcessStats,
    openProcessDetail,
    toggleScreenshotSelection,
    requestSoftDelete,
    handleConfirmSoftDelete,
    handleCancelSoftDelete,
    loadProcessMonthPage,
  };
}
