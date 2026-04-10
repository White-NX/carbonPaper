import React, { useState, useEffect, useMemo, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { listen } from '@tauri-apps/api/event';
import { Dialog } from '../Dialog';
import { ConfirmDialog } from '../ConfirmDialog';
import MigrationProgressDialog from './MigrationProgressDialog';
import { HardDrive, RefreshCw, Clock, Database, Activity, FolderOpen, AlertTriangle, PieChart, ArrowLeft, Trash2, ChevronLeft, ChevronRight } from 'lucide-react';
import { formatBytes, formatTimestamp } from './analysisUtils';
import { ThumbnailCard } from '../ThumbnailCard';
import {
  fetchThumbnailBatch,
  getProcessStorageStats,
  getProcessMonthlyThumbnails,
  getSoftDeleteQueueStatus,
  softDeleteScreenshots,
  softDeleteProcessMonth,
} from '../../lib/monitor_api';

const PROCESS_PALETTE = ['#0ea5e9', '#22c55e', '#f59e0b', '#ef4444', '#06b6d4', '#84cc16', '#8b5cf6', '#f97316'];

// Storage Ring Chart Component
function StorageRingChart({ totalDiskUsed, totalDiskSize, appUsedBytes, loading }) {
  const size = 180;
  const strokeWidth = 18;
  const radius = (size - strokeWidth) / 2;
  const circumference = 2 * Math.PI * radius;

  // Calculate percentages
  const diskUsagePercent = totalDiskSize > 0 ? Math.min((totalDiskUsed / totalDiskSize) * 100, 100) : 0;
  const appUsagePercent = totalDiskSize > 0 ? Math.min((appUsedBytes / totalDiskSize) * 100, 100) : 0;

  // Calculate stroke dash offsets
  const diskStrokeDashoffset = circumference - (diskUsagePercent / 100) * circumference;
  const appStrokeDashoffset = circumference - (appUsagePercent / 100) * circumference;

  const { t } = useTranslation();

  return (
    <div className="relative flex items-center justify-center">
      <svg width={size} height={size} className="transform -rotate-90">
        {/* Background ring */}
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius}
          stroke="currentColor"
          strokeWidth={strokeWidth}
          fill="none"
          className="text-ide-border/30"
        />
        {/* Disk usage ring (purple) */}
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius}
          stroke="url(#diskGradient)"
          strokeWidth={strokeWidth}
          fill="none"
          strokeDasharray={circumference}
          strokeDashoffset={diskStrokeDashoffset}
          strokeLinecap="round"
          className="transition-all duration-500"
        />
        {/* App usage ring (blue) */}
        <circle
          cx={size / 2}
          cy={size / 2}
          r={radius - strokeWidth - 4}
          stroke="url(#appGradient)"
          strokeWidth={strokeWidth - 4}
          fill="none"
          strokeDasharray={circumference * ((radius - strokeWidth - 4) / radius)}
          strokeDashoffset={(circumference * ((radius - strokeWidth - 4) / radius)) - (appUsagePercent / 100) * (circumference * ((radius - strokeWidth - 4) / radius))}
          strokeLinecap="round"
          className="transition-all duration-500"
        />
        <defs>
          <linearGradient id="diskGradient" x1="0%" y1="0%" x2="100%" y2="0%">
            <stop offset="0%" stopColor="#8B5CF6" />
            <stop offset="100%" stopColor="#A78BFA" />
          </linearGradient>
          <linearGradient id="appGradient" x1="0%" y1="0%" x2="100%" y2="0%">
            <stop offset="0%" stopColor="#3B82F6" />
            <stop offset="100%" stopColor="#60A5FA" />
          </linearGradient>
        </defs>
      </svg>
      <div className="absolute inset-0 flex flex-col items-center justify-center text-center">
        {loading ? (
          <RefreshCw className="w-6 h-6 animate-spin text-ide-muted" />
        ) : (
          <>
            <span className="text-2xl font-bold">{formatBytes(appUsedBytes)}</span>
            <span className="text-xs text-ide-muted">{t('settings.storageManagement.overview.program_used')}</span>
          </>
        )}
      </div>
    </div>
  );
}

function ProcessDistributionProgress({ stats, loading }) {
  const { t } = useTranslation();
  const total = (stats || []).reduce((sum, item) => sum + (item.screenshot_count || 0), 0);
  const topStats = (stats || []).slice(0, 8).map((item) => ({
    ...item,
    percent: total > 0 ? ((item.screenshot_count || 0) / total) * 100 : 0,
  }));
  const othersCount = (stats || []).slice(8).reduce((sum, item) => sum + (item.screenshot_count || 0), 0);
  const segments = othersCount > 0
    ? [...topStats, { process_name: t('settings.storageManagement.processDetails.others'), screenshot_count: othersCount, percent: total > 0 ? (othersCount / total) * 100 : 0 }]
    : topStats;

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between text-xs text-ide-muted">
        <span>{t('settings.storageManagement.processDetails.distributionTitle')}</span>
        {!loading && <span>{t('settings.storageManagement.processDetails.totalScreenshots', { count: total })}</span>}
      </div>

      {loading && (
        <div className="py-4 flex items-center justify-center">
          <RefreshCw className="w-5 h-5 animate-spin text-ide-muted" />
        </div>
      )}

      {!loading && topStats.length === 0 && (
        <div className="text-xs text-ide-muted py-2">{t('settings.storageManagement.processDetails.noStats')}</div>
      )}

      {!loading && topStats.length > 0 && (
        <div className="space-y-3">
          <div className="h-5 rounded-full overflow-hidden bg-ide-bg/70 flex">
            {segments.map((item, idx) => {
              const percent = Number(item.percent || 0);
              if (percent <= 0) return null;
              return (
                <div
                  key={`${item.process_name || 'unknown'}-${idx}`}
                  className="h-full transition-all duration-500"
                  style={{
                    width: `${Math.max(1, percent)}%`,
                    backgroundColor: PROCESS_PALETTE[idx % PROCESS_PALETTE.length],
                  }}
                  title={`${item.process_name || t('settings.storageManagement.processDetails.unknownProcess')} ${percent.toFixed(2)}%`}
                />
              );
            })}
          </div>

          <div className="grid grid-cols-1 sm:grid-cols-2 gap-2">
            {segments.map((item, idx) => {
              const percent = Number(item.percent || 0);
              return (
                <div key={`${item.process_name || 'unknown'}-legend-${idx}`} className="flex items-center justify-between gap-2 text-xs">
                  <div className="flex items-center gap-2 min-w-0">
                    <span
                      className="w-2.5 h-2.5 rounded-full shrink-0"
                      style={{ backgroundColor: PROCESS_PALETTE[idx % PROCESS_PALETTE.length] }}
                    />
                    <span className="truncate">{item.process_name || t('settings.storageManagement.processDetails.unknownProcess')}</span>
                  </div>
                  <span className="text-ide-muted shrink-0">{t('settings.storageManagement.processDetails.itemSummary', { count: item.screenshot_count || 0, percent: percent.toFixed(2) })}</span>
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}

// Storage option selector component
function StorageOptionSelect({ label, value, options, onChange, icon: Icon, description, className = '' }) {
  return (
    <div className={`bg-ide-bg/70 border border-ide-border rounded-xl p-4 ${className}`}>
      <div className="flex items-center gap-3 mb-3">
        {Icon && (
          <div className="p-2 rounded-lg bg-ide-panel border border-ide-border">
            <Icon className="w-4 h-4" />
          </div>
        )}
        <div className="flex-1">
          <div className="font-medium text-sm">{label}</div>
          {description && <div className="text-xs text-ide-muted mt-0.5">{description}</div>}
        </div>
      </div>
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="w-full bg-ide-panel border border-ide-border rounded-lg px-3 py-2 text-sm text-ide-text focus:outline-none focus:border-ide-accent cursor-pointer"
      >
        {options.map((opt) => (
          <option key={opt.value} value={opt.value}>
            {opt.label}
          </option>
        ))}
      </select>
    </div>
  );
}

function StoragePathOption({
  label,
  value,
  onChangePath,
  icon: Icon,
  description,
  error,
  disabled,
  className = '',
}) {
  const { t } = useTranslation();
  return (
    <div className={`bg-ide-bg/70 border border-ide-border rounded-xl p-4 ${className}`}>
      <div className="flex items-center gap-3 mb-3">
        {Icon && (
          <div className="p-2 rounded-lg bg-ide-panel border border-ide-border">
            <Icon className="w-4 h-4" />
          </div>
        )}
        <div className="flex-1">
          <div className="font-medium text-sm">{label}</div>
          {description && <div className="text-xs text-ide-muted mt-0.5">{description}</div>}
        </div>
      </div>
      <div className="flex items-center gap-2">
        <input
          type="text"
          disabled
          value={value || '--'}
          className="flex-1 bg-ide-panel border border-ide-border rounded-lg px-3 py-2 text-sm text-ide-muted truncate disabled:opacity-100 disabled:cursor-not-allowed"
        />
        <button
          type="button"
          onClick={onChangePath}
          disabled={disabled}
          className="shrink-0 px-3 py-2 text-xs border border-ide-border rounded-lg bg-ide-panel hover:border-ide-accent hover:text-ide-accent transition-colors disabled:opacity-60"
        >
          {disabled ? t('settings.storageManagement.storagePath.changing') : t('settings.storageManagement.storagePath.label')}
        </button>
      </div>
      {error && <div className="mt-2 text-xs text-ide-error">{error}</div>}
    </div>
  );
}

export default function StorageManagementSection({
  storageSegments,
  totalStorage,
  storage,
  loading,
  refreshing,
  error,
  onRefresh,
}) {
  // Storage settings from localStorage
  const [storageLimit, setStorageLimit] = useState(() => {
    return localStorage.getItem('snapshotStorageLimit') || 'unlimited';
  });

  const [retentionPeriod, setRetentionPeriod] = useState(() => {
    return localStorage.getItem('snapshotRetentionPeriod') || 'permanent';
  });

  const { t } = useTranslation();

  // Migration state
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
  const [deleteQueueStatus, setDeleteQueueStatus] = useState({
    pending_screenshots: 0,
    pending_ocr: 0,
    running: false,
  });

  // Save settings to localStorage
  useEffect(() => {
    localStorage.setItem('snapshotStorageLimit', storageLimit);
    // try to persist to backend; if backend not available, fall back to localStorage
    (async () => {
      try {
        await invoke('storage_set_policy', { policy: { storage_limit: storageLimit, retention_period: retentionPeriod } });
      } catch (e) {
        // backend may be unavailable in dev; ignore and rely on localStorage
      }
    })();
  }, [storageLimit]);

  useEffect(() => {
    localStorage.setItem('snapshotRetentionPeriod', retentionPeriod);
    // persist to backend as well
    (async () => {
      try {
        await invoke('storage_set_policy', { policy: { storage_limit: storageLimit, retention_period: retentionPeriod } });
      } catch (e) {
        // ignore
      }
    })();
  }, [retentionPeriod]);

  // On mount try to read policy from backend and sync UI
  useEffect(() => {
    (async () => {
      try {
        const resp = await invoke('storage_get_policy');
        if (resp && typeof resp === 'object') {
          const backendPolicy = resp;
          if (backendPolicy.storage_limit) setStorageLimit(String(backendPolicy.storage_limit));
          if (backendPolicy.retention_period) setRetentionPeriod(String(backendPolicy.retention_period));
        }
      } catch (e) {
        // backend not available — keep localStorage values
      }
    })();
  }, []);

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

  const handleRefresh = useCallback(() => {
    onRefresh?.();
    loadDeleteQueueStatus();
    if (panelView === 'overview') {
      loadProcessStats();
    }
    if (panelView === 'process-detail' && selectedProcess) {
      loadProcessMonthPage(selectedProcess, processPage);
    }
  }, [loadDeleteQueueStatus, loadProcessMonthPage, loadProcessStats, onRefresh, panelView, processPage, selectedProcess]);

  // Storage limit options
  const storageLimitOptions = [
    { value: '10', label: t('settings.storageManagement.storageLimit.options.10') },
    { value: '20', label: t('settings.storageManagement.storageLimit.options.20') },
    { value: '50', label: t('settings.storageManagement.storageLimit.options.50') },
    { value: '120', label: t('settings.storageManagement.storageLimit.options.120') },
    { value: 'unlimited', label: t('settings.storageManagement.storageLimit.options.unlimited') },
  ];

  // Retention period options
  const retentionOptions = [
    { value: '1month', label: t('settings.storageManagement.retention.options.1month') },
    { value: '6months', label: t('settings.storageManagement.retention.options.6months') },
    { value: '1year', label: t('settings.storageManagement.retention.options.1year') },
    { value: '2years', label: t('settings.storageManagement.retention.options.2years') },
    { value: 'permanent', label: t('settings.storageManagement.retention.options.permanent') },
  ];

  // Mock disk info - in real implementation this would come from backend
  const diskInfo = useMemo(() => {
    // Extract disk path from storage root path
    const rootPath = storage?.root_path || '';
    const driveLetter = rootPath.charAt(0);

    // For demo purposes, estimate disk usage
    // In production, this would come from a Rust backend call
    return {
      driveLetter: driveLetter || 'C',
      totalSize: 500 * 1024 * 1024 * 1024, // 500GB placeholder
      usedSize: 320 * 1024 * 1024 * 1024,   // 320GB placeholder
    };
  }, [storage]);

  const currentStoragePath = storage?.root_path || 'LocalAppData/CarbonPaper';

  const executeStoragePathChange = async (targetPath, shouldMigrateData) => {
    let unlistenProgress = null;
    let unlistenError = null;
    let shouldRestartMonitor = true;

    try {
      if (!targetPath) return;

      setMigrationError('');
      setIsUpdatingStoragePath(true);
      try {
        const monitorStatusRaw = await invoke('get_monitor_status');
        const monitorStatus = typeof monitorStatusRaw === 'string' ? JSON.parse(monitorStatusRaw) : monitorStatusRaw;
        shouldRestartMonitor = !monitorStatus?.stopped;
      } catch (_) {
        shouldRestartMonitor = true;
      }

      if (shouldRestartMonitor) {
        await invoke('stop_monitor');
      }

      if (shouldMigrateData) {
        setIsMigrating(true);
        setMigrationProgress({ total_files: 0, copied_files: 0, current_file: '' });

        unlistenProgress = await listen('storage-migration-progress', (evt) => {
          const p = evt.payload;
          setMigrationProgress(p);
        });

        unlistenError = await listen('storage-migration-error', (evt) => {
          setMigrationError(evt.payload?.message || t('settings.storageManagement.migration.error_default'));
        });
      }

      await invoke('storage_migrate_data_dir', {
        target: targetPath,
        migrateDataFiles: shouldMigrateData,
      });

      if (shouldMigrateData) {
        setMigrationProgress((s) => ({ ...s, current_file: t('settings.storageManagement.migration.completed') }));
        await new Promise((resolve) => setTimeout(resolve, 600));
      }

      onRefresh?.();
    } catch (e) {
      console.error('change storage path failed', e);
      setMigrationError(String(e));
    } finally {
      if (unlistenProgress) {
        try { await unlistenProgress(); } catch (_) { }
      }
      if (unlistenError) {
        try { await unlistenError(); } catch (_) { }
      }

      setIsMigrating(false);
      setIsUpdatingStoragePath(false);
      if (shouldRestartMonitor) {
        try { await invoke('start_monitor'); } catch (_) { }
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

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center justify-between shrink-0">
        <div className="space-y-1">
          <h2 className="text-xl font-semibold">{t('settings.storageManagement.title')}</h2>
          <p className="text-xs text-ide-muted">{t('settings.storageManagement.description')}</p>
        </div>
        <button
          type="button"
          onClick={handleRefresh}
          disabled={refreshing}
          className="flex items-center gap-2 px-3 py-2 text-xs border border-ide-border rounded-lg bg-ide-panel hover:border-ide-accent hover:text-ide-accent transition-colors disabled:opacity-60"
        >
          <RefreshCw className={`w-3.5 h-3.5 ${refreshing ? 'animate-spin' : ''}`} />
          {t('settings.storageManagement.refresh')}
        </button>
      </div>

      {error && (
        <div className="shrink-0 px-4 py-2 rounded-lg border border-red-500/40 text-xs text-red-200 bg-red-500/10">
          {error}
        </div>
      )}

      {panelView === 'overview' && (
        <>
          {/* Storage Ring Chart Card */}
          <div className="bg-ide-panel/60 border border-ide-border rounded-2xl p-6">
            <div className="flex items-center gap-2 mb-6">
              <div className="p-2 rounded-lg bg-ide-bg border border-ide-border">
                <HardDrive className="w-4 h-4" />
              </div>
              <div>
                <h3 className="font-semibold">{t('settings.storageManagement.overview.title')}</h3>
                <p className="text-[11px] text-ide-muted">{storage?.root_path || t('settings.storageManagement.overview.path', { path: 'LocalAppData/CarbonPaper' })}</p>
              </div>
            </div>

            <div className="flex flex-col lg:flex-row items-center gap-8">
              <div className="flex-shrink-0">
                <StorageRingChart
                  totalDiskUsed={diskInfo.usedSize}
                  totalDiskSize={diskInfo.totalSize}
                  appUsedBytes={totalStorage}
                  loading={loading}
                />
              </div>

              <div className="flex-1 space-y-4">
                <div className="space-y-2">
                  <div className="flex items-center gap-3">
                    <div className="w-3 h-3 rounded-full bg-gradient-to-r from-purple-500 to-purple-400" />
                    <span className="text-sm text-ide-muted">{t('settings.storageManagement.overview.disk_used')}</span>
                    <span className="text-sm font-medium ml-auto">{formatBytes(diskInfo.usedSize)}</span>
                  </div>
                  <div className="flex items-center gap-3">
                    <div className="w-3 h-3 rounded-full bg-gradient-to-r from-blue-500 to-blue-400" />
                    <span className="text-sm text-ide-muted">{t('settings.storageManagement.overview.program_used')}</span>
                    <span className="text-sm font-medium ml-auto">{formatBytes(totalStorage)}</span>
                  </div>
                </div>

                <div className="grid grid-cols-2 gap-2 pt-2 border-t border-ide-border/50">
                  {storageSegments.map((segment) => {
                    const Icon = segment.icon;
                    return (
                      <div key={segment.key} className="flex items-center gap-2 text-xs">
                        <Icon className="w-3.5 h-3.5 text-ide-muted" />
                        <span className="text-ide-muted">{segment.label}</span>
                        <span className="ml-auto font-medium">{formatBytes(segment.bytes)}</span>
                      </div>
                    );
                  })}
                </div>

                <div className="text-[11px] text-ide-muted pt-2">
                  {t('settings.storageManagement.last_updated', { time: storage?.cached_at_ms ? formatTimestamp(storage.cached_at_ms) : '--' })}
                </div>
              </div>
            </div>
          </div>

          <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
            <StoragePathOption
              className="md:col-span-2"
              label={t('settings.storageManagement.storagePath.label')}
              description={t('settings.storageManagement.storagePath.description')}
              value={currentStoragePath}
              onChangePath={handleChangeStoragePath}
              icon={FolderOpen}
              error={migrationError}
              disabled={isUpdatingStoragePath || isMigrating}
            />

            <StorageOptionSelect
              label={t('settings.storageManagement.storageLimit.label')}
              description={t('settings.storageManagement.storageLimit.description')}
              value={storageLimit}
              onChange={setStorageLimit}
              options={storageLimitOptions}
              icon={Database}
            />

            <StorageOptionSelect
              label={t('settings.storageManagement.retention.label')}
              description={t('settings.storageManagement.retention.description')}
              value={retentionPeriod}
              onChange={setRetentionPeriod}
              options={retentionOptions}
              icon={Clock}
            />
          </div>

          <div className="bg-ide-panel/60 border border-ide-border rounded-2xl p-5">
            <div className="flex items-center justify-between gap-3">
              <div className="flex items-center gap-3">
                <div className="p-2 rounded-lg bg-ide-bg border border-ide-border">
                  <PieChart className="w-4 h-4" />
                </div>
                <div>
                  <h3 className="font-semibold">{t('settings.storageManagement.processDetails.title')}</h3>
                  <p className="text-xs text-ide-muted">{t('settings.storageManagement.processDetails.description')}</p>
                </div>
              </div>
            </div>

            {deleteQueueStatus.running && (
              <div className="mt-4 text-xs text-ide-muted">
                {t('settings.storageManagement.processDetails.queueRunning', {
                  ocr: deleteQueueStatus.pending_ocr || 0,
                  screenshots: deleteQueueStatus.pending_screenshots || 0,
                })}
              </div>
            )}

            {processStatsError && (
              <div className="mt-4 text-xs text-red-400">{processStatsError}</div>
            )}

            <div className="mt-4 border border-ide-border rounded-xl p-4 bg-ide-bg/50">
              <ProcessDistributionProgress stats={processStats} loading={processStatsLoading} />
            </div>

            <div className="mt-4 space-y-2 overflow-y-auto pr-1">
              {(processStats || []).map((item, idx) => {
                const key = item.process_name || `unknown-${idx}`;
                const percent = Number(item.percentage || 0).toFixed(2);
                const hasProcessName = Boolean(item.process_name);
                return (
                  <button
                    key={key}
                    type="button"
                    disabled={!hasProcessName}
                    onClick={() => openProcessDetail(item.process_name)}
                    className="w-full text-left border border-ide-border rounded-xl p-3 bg-ide-bg/70 transition-colors hover:border-ide-accent/70 focus:outline-none focus:ring-1 focus:ring-ide-accent/40 disabled:opacity-60 disabled:cursor-not-allowed"
                  >
                    <div className="flex items-center justify-between gap-2">
                      <div className="min-w-0">
                        <div className="text-sm font-medium truncate">{item.process_name || t('settings.storageManagement.processDetails.unknownProcess')}</div>
                        <div className="text-xs text-ide-muted">{t('settings.storageManagement.processDetails.itemSummary', { count: item.screenshot_count || 0, percent })}</div>
                      </div>
                      {hasProcessName && <ChevronRight className="w-4 h-4 text-ide-muted shrink-0" />}
                    </div>
                    <div className="mt-2 h-1.5 bg-ide-panel rounded-full overflow-hidden">
                      <div
                        className="h-full"
                        style={{
                          width: `${Math.max(2, Number(item.percentage || 0))}%`,
                          backgroundColor: PROCESS_PALETTE[idx % PROCESS_PALETTE.length],
                        }}
                      />
                    </div>
                  </button>
                );
              })}

              {!processStatsLoading && (!processStats || processStats.length === 0) && (
                <div className="text-xs text-ide-muted">{t('settings.storageManagement.processDetails.noStats')}</div>
              )}
            </div>
          </div>

          {storageLimit === 'unlimited' && retentionPeriod === 'permanent' && (
            <div className="flex items-start gap-3 px-4 py-3 rounded-lg border border-ide-warning-border bg-ide-warning-bg">
              <AlertTriangle className="w-4 h-4 text-ide-warning mt-0.5 shrink-0" />
              <div className="text-xs text-yellow-600 dark:text-yellow-500">
                <p className="font-medium mb-1">{t('settings.storageManagement.warning.title')}</p>
                <p>{t('settings.storageManagement.warning.message')}</p>
              </div>
            </div>
          )}
        </>
      )}

      {panelView === 'process-detail' && (
        <div className="bg-ide-panel/60 border border-ide-border rounded-2xl p-5 space-y-4">
          <div className="flex items-center justify-between gap-2">
            <div className="flex items-center gap-2">
              <button
                type="button"
                onClick={() => setPanelView('overview')}
                className="inline-flex items-center gap-2 px-3 py-2 text-xs border border-ide-border rounded-lg bg-ide-panel hover:border-ide-accent hover:text-ide-accent transition-colors"
              >
                <ArrowLeft className="w-3.5 h-3.5" /> {t('settings.storageManagement.processDetails.back')}
              </button>
              <div>
                <div className="font-semibold text-sm">{selectedProcess || t('settings.storageManagement.processDetails.unknownProcess')}</div>
                <div className="text-xs text-ide-muted">{t('settings.storageManagement.processDetails.detailSubtitle')}</div>
              </div>
            </div>

            <button
              type="button"
              onClick={() => requestSoftDelete(selectedProcess, null)}
              disabled={deletingTarget === `${selectedProcess}::all`}
              className="inline-flex items-center gap-1 px-2 py-1 text-xs rounded-lg border border-red-500/40 text-red-300 hover:bg-red-500/10 disabled:opacity-60"
            >
              <Trash2 className="w-3.5 h-3.5" />
              {deletingTarget === `${selectedProcess}::all` ? t('settings.storageManagement.processDetails.deleting') : t('settings.storageManagement.processDetails.deleteProcess')}
            </button>
          </div>

          {processMonthError && <div className="text-xs text-red-400">{processMonthError}</div>}

          {processMonthLoading && (
            <div className="text-xs text-ide-muted inline-flex items-center gap-2">
              <RefreshCw className="w-3.5 h-3.5 animate-spin" /> {t('settings.storageManagement.processDetails.loading')}
            </div>
          )}

          {!processMonthLoading && groupedMonthItems.length === 0 && (
            <div className="text-xs text-ide-muted">{t('settings.storageManagement.processDetails.empty')}</div>
          )}

          {groupedMonthItems.map(([month, items]) => {
            const monthDeleteKey = `${selectedProcess}::${month}`;
            const deletingMonth = deletingTarget === monthDeleteKey;
            const monthDeletable = /^\d{4}-\d{2}$/.test(month);
            const selectedInMonth = items
              .map((item) => item.screenshot_id)
              .filter((id) => selectedScreenshotIds.has(id));
            const selectedCount = selectedCountByMonth[month] || 0;
            return (
              <div key={month} className="space-y-2">
                <div className="flex items-center justify-between">
                  <div className="text-sm font-medium">{month}</div>
                  <button
                    type="button"
                    onClick={() => requestSoftDelete(selectedProcess, month, selectedInMonth)}
                    disabled={deletingMonth || (selectedCount === 0 && !monthDeletable)}
                    className="inline-flex items-center gap-1 px-2 py-1 text-xs rounded-lg border border-red-500/40 text-red-300 hover:bg-red-500/10 disabled:opacity-60"
                  >
                    <Trash2 className="w-3.5 h-3.5" />
                    {deletingMonth
                        ? t('settings.storageManagement.processDetails.deleting')
                      : selectedCount > 0
                          ? t('settings.storageManagement.processDetails.deleteSelected', { count: selectedCount })
                          : t('settings.storageManagement.processDetails.deleteMonth')}
                  </button>
                </div>

                  <div className="grid grid-cols-3 gap-2">
                  {items.map((item) => {
                    const selected = selectedScreenshotIds.has(item.screenshot_id);
                    const thumbSrc = processThumbMap?.[String(item.screenshot_id)] || null;
                    return (
                      <div
                        key={item.screenshot_id}
                        className={`relative rounded ${selected ? 'ring-2 ring-ide-accent/80' : ''}`}
                        title={item.created_at}
                      >
                        <ThumbnailCard
                          item={{
                            screenshot_id: item.screenshot_id,
                            image_path: item.image_path,
                            process_name: selectedProcess,
                            window_title: item.created_at,
                            created_at: item.created_at,
                          }}
                          preloadedSrc={thumbSrc}
                          footerText={item.created_at}
                          footerPersistent
                          onSelect={(payload) => {
                            const id = payload?.screenshot_id ?? payload?.id;
                            toggleScreenshotSelection(id);
                          }}
                        />
                        {selected && (
                          <div className="pointer-events-none absolute top-1.5 left-1.5 px-1.5 py-0.5 rounded text-[10px] font-medium bg-ide-accent text-white">
                            {t('settings.storageManagement.processDetails.selected')}
                          </div>
                        )}
                      </div>
                    );
                  })}
                </div>
              </div>
            );
          })}

          <div className="flex items-center justify-end gap-2">
            <button
              type="button"
              onClick={() => loadProcessMonthPage(selectedProcess, Math.max(0, processPage - 1))}
              disabled={processPage <= 0 || processMonthLoading}
              className="inline-flex items-center gap-1 px-2 py-1 text-xs rounded-lg border border-ide-border bg-ide-panel disabled:opacity-60"
            >
              <ChevronLeft className="w-3.5 h-3.5" /> {t('settings.storageManagement.processDetails.prevPage')}
            </button>
            <div className="text-xs text-ide-muted">{t('settings.storageManagement.processDetails.page', { page: processPage + 1 })}</div>
            <button
              type="button"
              onClick={() => {
                if (processMonthData?.next_page !== null && processMonthData?.next_page !== undefined) {
                  loadProcessMonthPage(selectedProcess, processMonthData.next_page);
                }
              }}
              disabled={processMonthData?.next_page === null || processMonthData?.next_page === undefined || processMonthLoading}
              className="inline-flex items-center gap-1 px-2 py-1 text-xs rounded-lg border border-ide-border bg-ide-panel disabled:opacity-60"
            >
              {t('settings.storageManagement.processDetails.nextPage')} <ChevronRight className="w-3.5 h-3.5" />
            </button>
          </div>
        </div>
      )}
      {/* Migration progress dialog */}
      <ConfirmDialog
        isOpen={Boolean(pendingDeleteIntent)}
        onCancel={handleCancelSoftDelete}
        onConfirm={handleConfirmSoftDelete}
        title={pendingDeleteIntent?.title || t('settings.storageManagement.deleteConfirm.title')}
        message={pendingDeleteIntent?.message || ''}
        confirmLabel={pendingDeleteIntent?.confirmLabel || t('settings.storageManagement.deleteConfirm.confirmDefault')}
        cancelLabel={t('settings.storageManagement.deleteConfirm.cancel')}
        confirmVariant="danger"
        loading={Boolean(pendingDeleteIntent && deletingTarget === pendingDeleteIntent.targetKey)}
      />

      <MigrationProgressDialog
        isOpen={isMigrating}
        onClose={() => { /* prevent closing while migrating */ }}
        progress={migrationProgress}
        error={migrationError}
      />
      <Dialog
        isOpen={isMigrationChoiceDialogOpen}
        onClose={handleCancelMigrationChoice}
        title={t('settings.storageManagement.storagePath.changeTitle')}
        maxWidth="max-w-md"
      >
        <div className="p-4 space-y-3">
          <div className="text-sm text-ide-text">{t('settings.storageManagement.storagePath.selectedPath')}</div>
          <div className="px-3 py-2 rounded-lg border border-ide-border bg-ide-panel text-xs text-ide-muted break-all">
            {pendingTargetPath || '--'}
          </div>
          <p className="text-xs text-ide-muted">{t('settings.storageManagement.storagePath.migrateQuestion')}</p>
          <div className="flex items-center justify-end gap-2 pt-2">
            <button
              type="button"
              onClick={handleCancelMigrationChoice}
              className="px-3 py-1.5 text-xs border border-ide-border rounded-lg bg-ide-panel hover:border-ide-accent hover:text-ide-accent transition-colors"
            >
              {t('settings.storageManagement.storagePath.cancel')}
            </button>
            <button
              type="button"
              onClick={() => handleApplyStoragePath(false)}
              className="px-3 py-1.5 text-xs border border-ide-border rounded-lg bg-ide-panel hover:border-ide-accent hover:text-ide-accent transition-colors"
            >
              {t('settings.storageManagement.storagePath.applyPath')}
            </button>
            <button
              type="button"
              onClick={() => handleApplyStoragePath(true)}
              className="px-3 py-1.5 text-xs rounded-lg bg-ide-accent hover:bg-ide-accent/90 text-white transition-colors"
            >
              {t('settings.storageManagement.storagePath.migrateAndApply')}
            </button>
          </div>
        </div>
      </Dialog>

    </div>
  );
}

