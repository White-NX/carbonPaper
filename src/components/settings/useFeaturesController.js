import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../lib/auth_api';
import { getClusteringStatus, runClustering, saveClusteringResults } from '../../lib/task_api';
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
  const [clusteringError, setClusteringError] = useState(null);
  const [clusteringNotice, setClusteringNotice] = useState(null);
  const [clusteringStatus, setClusteringStatus] = useState(null);
  const [rangeStart, setRangeStart] = useState('');
  const [rangeEnd, setRangeEnd] = useState('');
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

  const refreshClusteringStatus = async () => {
    if (monitorStatus !== 'running') return;
    try {
      const result = await getClusteringStatus();
      if (result?.status === 'success') {
        setClusteringStatus(result);
      }
    } catch { /* ignore */ }
  };

  useEffect(() => {
    refreshClusteringStatus();
  }, [monitorStatus]);

  useEffect(() => {
    const handler = () => {
      setClusteringDropdownOpen(false);
    };
    if (clusteringDropdownOpen) {
      document.addEventListener('click', handler);
      return () => document.removeEventListener('click', handler);
    }
  }, [clusteringDropdownOpen]);

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
    setClusteringRunning(true);
    setClusteringError(null);
    setClusteringNotice(null);
    try {
      const options = { manual: true };
      if (rangeStart) options.startTime = new Date(rangeStart).getTime() / 1000;
      if (rangeEnd) options.endTime = new Date(rangeEnd).getTime() / 1000;

      let result = await runClustering(options);
      if (result?.status === 'needs_user_choice') {
        const hasCompleteRange = Boolean(rangeStart && rangeEnd);
        const count = result?.estimate?.count ?? result?.n_total ?? 0;
        const memory = result?.estimate?.memory || {};
        const scope = hasCompleteRange
          ? t('tasks.clusteringRangeScope')
          : t('tasks.clusteringAllScope');
        const reason = result.reason === 'low_memory'
          ? t('tasks.clusteringLowMemoryReason')
          : t('tasks.clusteringLargeRangeReason');
        const useBatched = window.confirm(t('tasks.clusteringDegradePrompt', {
          scope,
          count,
          reason,
          estimatedGb: memory.estimated_peak_bytes
            ? (memory.estimated_peak_bytes / (1024 ** 3)).toFixed(1)
            : '-',
        }));
        result = await runClustering({
          ...options,
          clusteringMode: useBatched ? 'batched' : 'full',
        });
      }

      if (result?.status === 'empty') {
        setClusteringError(t('tasks.noData'));
      }

      if (result?.clusters?.length) {
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
      }

      if (result?.degraded) {
        setClusteringNotice(t('tasks.clusteringDegradedNotice', {
          sampleSize: result.sample_size ?? 0,
          assignedCount: result.assigned_count ?? 0,
        }));
      }

      await refreshClusteringStatus();
    } catch (err) {
      const msg = String(err?.message || err);
      if (msg.includes('not found') || msg.includes('ModelNotAvailable') || msg.includes('not downloaded')) {
        setClusteringError(t('tasks.modelMissing'));
      } else {
        setClusteringError(msg);
      }
      console.error('Clustering failed:', err);
    } finally {
      setClusteringRunning(false);
    }
  };

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
    clusteringRunning,
    clusteringError,
    clusteringNotice,
    rangeStart,
    setRangeStart,
    rangeEnd,
    setRangeEnd,
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
