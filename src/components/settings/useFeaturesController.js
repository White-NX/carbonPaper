import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../lib/auth_api';
import { runClustering, saveClusteringResults } from '../../lib/task_api';
import { useClusteringStatus } from './organize/useClusteringStatus';
import { useModelInventory } from './organize/useModelInventory';
import { useSmartClusterControls } from './organize/useSmartClusterControls';

export function useFeaturesController({
  monitorStatus,
  t,
  featureModeDefinitions,
  getFeatureMode,
}) {
  const [config, setConfig] = useState(null);
  const [loading, setLoading] = useState(true);
  const [clusteringDropdownOpen, setClusteringDropdownOpen] = useState(false);
  const [clusteringAdvancedOpen, setClusteringAdvancedOpen] = useState(false);
  const [clusteringRunning, setClusteringRunning] = useState(false);
  const [clusteringPhase, setClusteringPhase] = useState(null);
  const [clusteringError, setClusteringError] = useState(null);
  const [clusteringNotice, setClusteringNotice] = useState(null);
  const { clusteringStatus, refreshClusteringStatus } = useClusteringStatus(monitorStatus, clusteringRunning);
  const [rangeStart, setRangeStart] = useState('');
  const [rangeEnd, setRangeEnd] = useState('');
  const [clusteringResourceChoice, setClusteringResourceChoice] = useState(null);
  const clusteringResourceChoiceResolver = useRef(null);
  const clusteringMounted = useRef(false);
  const clusteringRequestPending = useRef(false);
  const clusteringRetryOptions = useRef(null);
  const queuedClusteringAt = useRef(null);
  const [customControlsOpen, setCustomControlsOpen] = useState(false);
  const modelInventory = useModelInventory();
  const smartCluster = useSmartClusterControls();
  const { scModelAvailable } = smartCluster;

  const loadConfig = async () => {
    try {
      const result = await invoke('get_advanced_config');
      if (result.clustering_enabled === undefined) result.clustering_enabled = true;
      if (result.classification_enabled === undefined) result.classification_enabled = true;
      setConfig(result);
    } catch (err) {
      console.error('Failed to load advanced config:', err);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadConfig();
  }, []);

  useEffect(() => {
    const handler = () => {
      setClusteringDropdownOpen(false);
    };
    if (clusteringDropdownOpen) {
      document.addEventListener('click', handler);
      return () => document.removeEventListener('click', handler);
    }
  }, [clusteringDropdownOpen]);

  const requestClusteringResourceChoice = useCallback((choice) => new Promise((resolve) => {
    clusteringResourceChoiceResolver.current?.(false);
    clusteringResourceChoiceResolver.current = resolve;
    setClusteringResourceChoice(choice);
  }), []);

  const resolveClusteringResourceChoice = useCallback((useBatched) => {
    const resolve = clusteringResourceChoiceResolver.current;
    clusteringResourceChoiceResolver.current = null;
    setClusteringResourceChoice(null);
    resolve?.(useBatched);
  }, []);

  useEffect(() => {
    clusteringMounted.current = true;
    return () => {
      clusteringMounted.current = false;
      clusteringResourceChoiceResolver.current?.(false);
      clusteringResourceChoiceResolver.current = null;
    };
  }, []);

  const saveConfig = async (newConfig) => {
    setConfig(newConfig);
    try {
      await withAuth(() => invoke('set_advanced_config', { config: newConfig }), { autoPrompt: true });
      await withAuth(() => invoke('monitor_update_feature_config', {
        clusteringEnabled: newConfig.clustering_enabled,
        classificationEnabled: newConfig.classification_enabled,
      }), { autoPrompt: true });
    } catch (err) {
      console.error('Failed to save advanced config:', err);
    }
  };

  const handleFeatureModeChange = async (mode) => {
    if (!config) return;
    if (mode === 'smart' && !scModelAvailable) return;

    const option = featureModeDefinitions.find((item) => item.value === mode);
    if (!option) return;

    setCustomControlsOpen(false);
    await saveConfig({
      ...config,
      ...option.config,
    });
  };

  const handleCustomFeatureToggle = async (key) => {
    if (!config) return;
    if (key === 'smart_cluster_enabled' && !config.smart_cluster_enabled && !scModelAvailable) return;

    await saveConfig({
      ...config,
      [key]: !config[key],
    });
  };

  const handleClusteringIntervalChange = async (interval) => {
    if (!config) return;
    setClusteringDropdownOpen(false);
    const newConfig = { ...config, clustering_interval: interval };
    await saveConfig(newConfig);
    try {
      await withAuth(() => invoke('monitor_set_clustering_interval', { interval }), { autoPrompt: true });
    } catch {
      // Best-effort runtime update; persisted config still applies next run.
    }
  };

  const handleRunClustering = async () => {
    if (clusteringRequestPending.current || clusteringBusy) return;
    clusteringRequestPending.current = true;
    const requestedAt = Date.now();
    setClusteringRunning(true);
    setClusteringPhase('preparing');
    setClusteringError(null);
    setClusteringNotice(null);
    try {
      // Pin an open-ended history range so a retry uses the same checkpoint.
      const previous = clusteringRetryOptions.current;
      const options = previous?.rangeStart === rangeStart && previous?.rangeEnd === rangeEnd
        ? previous.options
        : { manual: true };
      if (rangeStart || rangeEnd) {
        options.startTime ??= rangeStart ? new Date(rangeStart).getTime() / 1000 : 0;
        options.endTime ??= rangeEnd ? new Date(rangeEnd).getTime() / 1000 : requestedAt / 1000;
      }
      clusteringRetryOptions.current = { rangeStart, rangeEnd, options };

      let result = await runClustering(options);
      if (result?.status === 'needs_user_choice') {
        if (!clusteringMounted.current) return;
        setClusteringPhase('awaiting_choice');
        const hasCompleteRange = options.startTime != null && options.endTime != null;
        const count = result?.estimate?.count ?? result?.n_total ?? 0;
        const memory = result?.estimate?.memory || {};
        const scope = hasCompleteRange
          ? t('tasks.clusteringRangeScope')
          : t('tasks.clusteringAllScope');
        const reason = result.reason === 'low_memory'
          ? t('tasks.clusteringLowMemoryReason')
          : t('tasks.clusteringLargeRangeReason');
        const useBatched = await requestClusteringResourceChoice({
          prompt: t('tasks.clusteringDegradePrompt', {
            scope,
            count,
            reason,
            estimatedGb: memory.estimated_peak_bytes
              ? (memory.estimated_peak_bytes / (1024 ** 3)).toFixed(1)
              : '-',
          }),
        });
        if (!clusteringMounted.current) return;
        setClusteringPhase('preparing');
        result = await runClustering({
          ...options,
          clusteringMode: useBatched ? 'batched' : 'full',
        });
      }

      if (result?.status === 'queued' || result?.queued) {
        queuedClusteringAt.current = requestedAt;
        setClusteringNotice(t(
          'settings.features.management.clustering.queued',
          '已加入后台整理队列',
        ));
        return;
      }

      if (result?.status === 'empty') {
        setClusteringError(t('tasks.noData'));
      } else if (result?.status === 'already_running') {
        throw new Error('CLUSTERING_ALREADY_RUNNING');
      }

      if (result?.clusters?.length) {
        setClusteringPhase('saving');
        const taskRequests = result.clusters.map((cl) => ({
          auto_label: cl.dominant_process || null,
          dominant_process: cl.dominant_process || null,
          dominant_category: cl.dominant_category || null,
          start_time: cl.start_time || null,
          end_time: cl.end_time || null,
          snapshot_count: cl.snapshot_count || 0,
          layer: 'hot',
          screenshot_ids: (cl.snapshot_ids || []).map((id) => Number(id)),
          confidences: null,
        }));
        await saveClusteringResults(taskRequests);
        setClusteringNotice(t('settings.features.management.clustering.completed', {
          count: taskRequests.length,
        }));
      } else if (result?.status === 'success') {
        setClusteringNotice(t('settings.features.management.clustering.progress.completed'));
      }

      if (result?.degraded) {
        setClusteringNotice(t('tasks.clusteringDegradedNotice', {
          sampleSize: result.sample_size ?? 0,
          assignedCount: result.assigned_count ?? 0,
        }));
      }

      clusteringRetryOptions.current = null;
    } catch (err) {
      const msg = String(err?.message || err);
      if (msg === 'CLUSTERING_FAILED') {
        setClusteringError(t('tasks.clusteringFailed'));
      } else if (msg.includes('not found') || msg.includes('ModelNotAvailable') || msg.includes('not downloaded')) {
        setClusteringError(t('tasks.modelMissing'));
      } else if (msg.includes('retry to resume') || msg.includes('retry clustering to resume')) {
        setClusteringError(t('settings.features.management.clustering.progress.interrupted'));
      } else if (msg.includes('CLUSTERING_ALREADY_RUNNING')) {
        setClusteringError(t('settings.features.management.clustering.progress.already_running'));
      } else {
        setClusteringError(msg);
      }
      console.error('Clustering failed:', err);
    } finally {
      await refreshClusteringStatus({ fresh: true });
      clusteringRequestPending.current = false;
      setClusteringRunning(false);
      setClusteringPhase(null);
    }
  };

  const run = clusteringStatus?.clustering_progress;
  const checkpoint = clusteringStatus?.vector_sync;
  const scheduledTask = clusteringStatus?.scheduler?.tasks?.find((task) => task.task_kind === 'python_clustering');
  const scheduledRunning = clusteringStatus?.scheduler?.running_task === 'python_clustering';
  const scheduledQueued = scheduledTask?.manual_pending && scheduledTask.status === 'queued';
  const clusteringBusy = Boolean(clusteringRunning || run?.active || scheduledRunning || scheduledQueued);
  let clusteringProgress = null;
  if (clusteringRunning && ['saving', 'awaiting_choice'].includes(clusteringPhase)) {
    clusteringProgress = { phase: clusteringPhase, active: clusteringPhase === 'saving' };
  } else if (run?.active) {
    clusteringProgress = run;
  } else if (clusteringRunning) {
    clusteringProgress = { phase: 'preparing', active: true };
  } else if (scheduledRunning) {
    clusteringProgress = { phase: 'preparing', active: true };
  } else if (scheduledQueued) {
    clusteringProgress = { phase: 'queued', active: true };
  } else if (scheduledTask?.manual_pending && scheduledTask.status === 'retry_wait') {
    clusteringProgress = { phase: 'retry_wait', active: false };
  } else if (run?.phase === 'waiting_for_index') {
    clusteringProgress = run;
  } else if (run?.manual && ['interrupted', 'completed'].includes(run.phase)) {
    clusteringProgress = run;
  } else if (!run && checkpoint?.scope?.startsWith('range:') && !checkpoint.complete) {
    clusteringProgress = { phase: 'interrupted', active: false, prepared_count: checkpoint.synced_count };
  }

  useEffect(() => {
    if (clusteringRunning || queuedClusteringAt.current == null || !scheduledTask) return;
    if (scheduledTask.status === 'completed' && scheduledTask.last_completed_at_ms >= queuedClusteringAt.current) {
      queuedClusteringAt.current = null;
      setClusteringNotice(t('settings.features.management.clustering.progress.completed'));
    } else if (scheduledTask.status === 'failed') {
      queuedClusteringAt.current = null;
      setClusteringNotice(null);
      setClusteringError(t('settings.features.management.clustering.progress.interrupted'));
    }
  }, [clusteringRunning, scheduledTask, t]);

  const lastClusteringRunLabel = clusteringStatus?.config?.last_run
    ? new Date(clusteringStatus.config.last_run * 1000).toLocaleString()
    : t('tasks.never');
  const featureMode = config ? getFeatureMode(config) : 'minimal';
  const featureModeOptions = featureModeDefinitions.map((option) => ({
    ...option,
    label: t(`settings.features.management.featureMode.options.${option.value}.label`),
    description: t(`settings.features.management.featureMode.options.${option.value}.description`),
    disabled: option.value === 'smart' && !scModelAvailable,
    title: option.value === 'smart' && !scModelAvailable
      ? t('settings.features.management.smartCluster.modelMissing', '请先下载模型')
      : t(`settings.features.management.featureMode.options.${option.value}.description`),
  }));
  const selectedFeatureMode = featureModeOptions.find((option) => option.value === featureMode) || featureModeOptions[0];

  useEffect(() => {
    if (featureMode === 'custom') {
      setCustomControlsOpen(true);
    }
  }, [featureMode]);

  return {
    config,
    loading,
    ...modelInventory,
    clusteringDropdownOpen,
    setClusteringDropdownOpen,
    clusteringAdvancedOpen,
    setClusteringAdvancedOpen,
    clusteringRunning: clusteringBusy,
    clusteringProgress,
    clusteringError,
    clusteringNotice,
    rangeStart,
    setRangeStart,
    rangeEnd,
    setRangeEnd,
    clusteringResourceChoice,
    resolveClusteringResourceChoice,
    customControlsOpen,
    setCustomControlsOpen,
    scModelAvailable,
    ...smartCluster,
    handleFeatureModeChange,
    handleCustomFeatureToggle,
    handleClusteringIntervalChange,
    handleRunClustering,
    clearClusteringError: () => setClusteringError(null),
    clearClusteringNotice: () => setClusteringNotice(null),
    lastClusteringRunLabel,
    featureMode,
    featureModeOptions,
    selectedFeatureMode,
  };
}
