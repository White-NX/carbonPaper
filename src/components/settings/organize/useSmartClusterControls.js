import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../../lib/auth_api';
import { useTauriEventListener } from '../../../hooks/useTauriEventListener';
import { useSettingsActive, useSettingsActivity } from '../SettingsActivityContext';
import { notifySettingsChanged } from '../../../lib/settings_api';

export function useSmartClusterControls() {
  const active = useSettingsActive();
  const [scModelAvailable, setScModelAvailable] = useState(false);
  const [scStatus, setScStatus] = useState(null);
  const [scDownloading, setScDownloading] = useState(false);
  const [scDownloadLog, setScDownloadLog] = useState([]);
  const [scDownloadError, setScDownloadError] = useState(null);
  const scDownloadStartedRef = useRef(false);
  useSettingsActivity('smart-component', { busy: scDownloading });

  const refreshSmartClusterModel = async () => {
    try {
      const modelStatus = await invoke('check_model_files');
      const reranker = modelStatus?.['bge-reranker-v2-m3'];
      setScModelAvailable(reranker?.complete === true);
    } catch (err) {
      console.warn('Failed to check reranker model:', err);
    }
  };

  const refreshSmartClusterStatus = async () => {
    try {
      const status = await withAuth(() => invoke('smart_cluster_status'));
      setScStatus(status);
    } catch {
      // ignore
    }
  };

  useEffect(() => {
    if (!active) return undefined;
    refreshSmartClusterModel();
    refreshSmartClusterStatus();
    const interval = setInterval(() => {
      refreshSmartClusterStatus();
    }, 10000);
    return () => clearInterval(interval);
  }, [active]);

  useTauriEventListener('install-log', (event) => {
    const line = event?.payload?.line || JSON.stringify(event?.payload || {});
    const ts = new Date().toLocaleTimeString();
    setScDownloadLog((prev) => [...prev, `[${ts}] ${line}`]);
  }, [scDownloading], scDownloading);

  const handleDownloadReranker = async () => {
    if (scDownloadStartedRef.current) return;
    scDownloadStartedRef.current = true;
    setScDownloading(true);
    setScDownloadLog([]);
    setScDownloadError(null);
    try {
      setScDownloadLog((prev) => [...prev, `[${new Date().toLocaleTimeString()}] Downloading bge-reranker-v2-m3 (uint8, ~570MB)...`]);
      await withAuth(
        () => invoke('download_model', { modelId: 'bge-reranker-v2-m3' }),
        { autoPrompt: true },
      );
      await invoke('mark_smart_cluster_setup_done', { dismissedPermanently: false });
      await refreshSmartClusterModel();
      await notifySettingsChanged(['models']);
    } catch (err) {
      setScDownloadError(err?.message || String(err));
      scDownloadStartedRef.current = false;
    } finally {
      setScDownloading(false);
      scDownloadStartedRef.current = false;
    }
  };

  const handleDrainNow = async () => {
    try {
      await withAuth(() => invoke('monitor_smart_cluster_drain_now'), { autoPrompt: true });
      setTimeout(refreshSmartClusterStatus, 500);
    } catch (err) {
      console.warn('Failed to trigger drain_now:', err);
    }
  };

  const handleRescanAll = async () => {
    try {
      await withAuth(() => invoke('smart_cluster_rescan_all'), { autoPrompt: true });
      setTimeout(refreshSmartClusterStatus, 500);
    } catch (err) {
      console.warn('Failed to trigger rescan:', err);
    }
  };

  return {
    scModelAvailable,
    scStatus,
    scDownloading,
    scDownloadLog,
    scDownloadError,
    handleDownloadReranker,
    handleDrainNow,
    handleRescanAll,
  };
}
