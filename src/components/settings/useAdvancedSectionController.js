import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { withAuth } from '../../lib/auth_api';
import { getClipBackfillOffer, setClipBackfillDecision } from '../../lib/task_api';

export function useAdvancedSectionController({ monitorStatus, t }) {
  const [config, setConfig] = useState(null);
  const [loading, setLoading] = useState(true);
  const [cpuDropdownOpen, setCpuDropdownOpen] = useState(false);
  const [gpuDropdownOpen, setGpuDropdownOpen] = useState(false);
  const [clusteringDropdownOpen, setClusteringDropdownOpen] = useState(false);
  const [cpuChanged, setCpuChanged] = useState(false);
  const [dmlChanged, setDmlChanged] = useState(false);
  const [onnxChanged, setOnnxChanged] = useState(false);
  const [gpus, setGpus] = useState([]);
  const [gpuLoading, setGpuLoading] = useState(false);
  const [vacuumRunning, setVacuumRunning] = useState(false);
  const [vacuumMessage, setVacuumMessage] = useState('');
  const [mlOcrStatus, setMlOcrStatus] = useState(null);
  const [mlOcrStatusLoading, setMlOcrStatusLoading] = useState(false);
  const [rustOcrModelStatus, setRustOcrModelStatus] = useState(null);
  const [rustOcrModelDownloading, setRustOcrModelDownloading] = useState(false);
  const [semanticStatus, setSemanticStatus] = useState(null);
  const [semanticStatusLoading, setSemanticStatusLoading] = useState(false);
  const [semanticIndexRunning, setSemanticIndexRunning] = useState(false);
  const [semanticIndexRun, setSemanticIndexRun] = useState(null);
  // The CLIP image index has its own run, its own progress event and its own
  // rollback lever. Kept separate from the MiniLM ones above rather than shared:
  // the two passes can be running at once, and one status line for both would
  // report whichever finished last.
  const [clipIndexRunning, setClipIndexRunning] = useState(false);
  const [clipIndexRun, setClipIndexRun] = useState(null);
  const [clipIndexStopping, setClipIndexStopping] = useState(false);
  const [clipIndexProgress, setClipIndexProgress] = useState(null);
  // Whether a backfill of everything the step-7 migration could not deliver has
  // been offered, and what the user said. The dialog asks once; this is where
  // the answer stays changeable, which is the whole reason declining is safe to
  // record durably.
  const [clipBackfill, setClipBackfill] = useState(null);
  const [clipBackfillBusy, setClipBackfillBusy] = useState(false);
  const [semanticIndexProgress, setSemanticIndexProgress] = useState(null);
  const [semanticIndexStopping, setSemanticIndexStopping] = useState(false);

  const saveConfig = async (newConfig) => {
    const previousConfig = config;
    setConfig(newConfig);
    try {
      await withAuth(() => invoke('set_advanced_config', { config: newConfig }), { autoPrompt: true });
      return true;
    } catch (err) {
      setConfig(previousConfig);
      console.error('Failed to save advanced config:', err);
      return false;
    }
  };

  const syncOcrConfigToMonitor = async (newConfig) => {
    if (monitorStatus !== 'running') return;
    try {
      await withAuth(() => invoke('monitor_update_advanced_config', {
        ocrTimeoutSecs: newConfig.ocr_timeout_secs || 120,
        clusteringAllowFullLowMemory: Boolean(newConfig.clustering_allow_full_low_memory),
      }), { autoPrompt: true });
    } catch (err) {
      console.error('Failed to sync OCR config to monitor:', err);
    }
  };

  const loadConfig = async () => {
    try {
      const result = await invoke('get_advanced_config');
      setConfig(result);
    } catch (err) {
      console.error('Failed to load advanced config:', err);
    } finally {
      setLoading(false);
    }
  };

  const loadGpus = async () => {
    setGpuLoading(true);
    try {
      const result = await invoke('enumerate_gpus');
      const gpuList = result || [];
      setGpus(gpuList);
      if (config && gpuList.length > 0 && !gpuList.some((gpu) => gpu.id === config.dml_device_id)) {
        await saveConfig({ ...config, dml_device_id: gpuList[0].id });
      }
    } catch (err) {
      console.error('Failed to enumerate GPUs:', err);
      setGpus([]);
    } finally {
      setGpuLoading(false);
    }
  };

  const refreshVacuumRunningStatus = async () => {
    try {
      const status = await invoke('storage_get_startup_vacuum_status');
      setVacuumRunning(Boolean(status?.in_progress));
    } catch {
      setVacuumRunning(false);
    }
  };

  useEffect(() => {
    loadConfig();
  }, []);

  useEffect(() => {
    if (config?.use_dml) {
      loadGpus();
    }
  }, [config?.use_dml]);

  useEffect(() => {
    const handler = () => {
      setCpuDropdownOpen(false);
      setGpuDropdownOpen(false);
      setClusteringDropdownOpen(false);
    };
    if (cpuDropdownOpen || gpuDropdownOpen || clusteringDropdownOpen) {
      document.addEventListener('click', handler);
      return () => document.removeEventListener('click', handler);
    }
    return undefined;
  }, [cpuDropdownOpen, gpuDropdownOpen, clusteringDropdownOpen]);

  useEffect(() => {
    refreshVacuumRunningStatus();
  }, []);

  const refreshMlOcrStatus = async () => {
    setMlOcrStatusLoading(true);
    try {
      setMlOcrStatus(await invoke('get_ml_ocr_status'));
    } catch (err) {
      console.warn('Failed to read Rust ML OCR status:', err);
    } finally {
      setMlOcrStatusLoading(false);
    }
  };

  const refreshRustOcrModelStatus = async () => {
    try {
      setRustOcrModelStatus(await invoke('get_rust_ocr_model_status'));
    } catch (err) {
      console.warn('Failed to read Rust OCR model status:', err);
    }
  };

  useEffect(() => {
    refreshRustOcrModelStatus();
  }, []);

  /**
   * Read the backend diagnostic, and with it whether a manual indexing run is
   * going.
   *
   * A run started from this hook is tracked by its own promise. A run this
   * hook did not start — the settings dialog was closed or switched away from
   * mid-run, which unmounts it — has no promise to await, so the backend flag
   * is what tells the reopened dialog that its button should say "stop". The
   * two must not fight over the same state, hence the per-index ownership refs.
   */
  const ownsRun = useRef(false);
  const ownsClipRun = useRef(false);

  const readSemanticStatus = async ({ quiet = false } = {}) => {
    if (!quiet) setSemanticStatusLoading(true);
    try {
      const status = await invoke('get_ml_semantic_status');
      setSemanticStatus(status);
      if (!ownsRun.current) {
        const active = Boolean(status?.backend?.index_run_active);
        setSemanticIndexRunning(active);
        if (!active) setSemanticIndexStopping(false);
      }
      if (!ownsClipRun.current) {
        const active = Boolean(status?.clip_backend?.index_run_active);
        setClipIndexRunning(active);
        if (!active) setClipIndexStopping(false);
      }
    } catch (err) {
      console.warn('Failed to read semantic backend status:', err);
    } finally {
      if (!quiet) setSemanticStatusLoading(false);
    }
  };

  // The refresh button hands its click event to whatever it calls, so the
  // public wrapper takes no arguments rather than letting an event object
  // arrive where the options object belongs.
  const refreshSemanticStatus = () => readSemanticStatus();

  useEffect(() => {
    refreshSemanticStatus();
  }, []);

  /**
   * Notice the end of either run this dialog does not own. Two seconds is far
   * shorter than the time it takes to wonder why a button is still disabled,
   * and the read itself is one ledger query. Not started for an owned run:
   * that one already refreshes when its command returns.
   */
  useEffect(() => {
    const watchingSemantic = semanticIndexRunning && !ownsRun.current;
    const watchingClip = clipIndexRunning && !ownsClipRun.current;
    if (!watchingSemantic && !watchingClip) return undefined;
    const timer = window.setInterval(() => readSemanticStatus({ quiet: true }), 2000);
    return () => window.clearInterval(timer);
  }, [semanticIndexRunning, clipIndexRunning]);

  /**
   * The one-release rollback lever. `chroma` hands retrieval of screenshot text
   * back to Python; the vectors stay where they are and Rust keeps writing
   * them, so it is reversible at any time and it does not stop indexing.
   * `semantic_runtime` stays a separate registry-only switch because it rolls
   * back Rust inference itself, which is a developer concern.
   */
  const handleToggleRustSemanticIndex = async (enabled) => {
    const newConfig = { ...config, semantic_index: enabled ? 'rust' : 'chroma' };
    const saved = await saveConfig(newConfig);
    if (saved) await refreshSemanticStatus();
  };

  /**
   * The same one-release rollback for visual search. `chroma` hands the
   * text-to-image query back to Python; Rust keeps encoding new captures and
   * keeps mirroring them into the Chroma collection, so the switch is
   * reversible at any time and does not stop indexing either.
   */
  const handleToggleRustClipIndex = async (enabled) => {
    const newConfig = { ...config, clip_index: enabled ? 'rust' : 'chroma' };
    const saved = await saveConfig(newConfig);
    if (saved) await refreshSemanticStatus();
  };

  /**
   * Progress of a running manual pass, one event per encoded chunk of four.
   *
   * Subscribed for the lifetime of the settings section rather than for the
   * lifetime of a run: the pass keeps going if this dialog is closed and
   * reopened, and a subscription tied to `semanticIndexRunning` would miss the
   * events that arrive in between.
   */
  useEffect(() => {
    let unlisten = null;
    let cancelled = false;
    listen('semantic-index-progress', (event) => {
      setSemanticIndexProgress(event.payload || null);
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    }).catch((err) => {
      console.warn('Failed to subscribe to semantic index progress:', err);
    });
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  /** The same subscription for the CLIP pass, for the same reason. */
  useEffect(() => {
    let unlisten = null;
    let cancelled = false;
    listen('clip-index-progress', (event) => {
      setClipIndexProgress(event.payload || null);
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    }).catch((err) => {
      console.warn('Failed to subscribe to CLIP index progress:', err);
    });
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  /**
   * The manual alternative to the idle gate. Capture-side indexing normally
   * waits for an idle window on mains power, which on a machine that is rarely
   * either can leave recent screenshots out of natural-language search
   * indefinitely.
   *
   * The run drains the whole queue, so it can last minutes on a deep backlog.
   * That is why it reports progress as it goes and why it can be stopped: the
   * bound that used to keep it short — 128 screenshots — made one click look
   * like it had done nothing and needed pressing dozens of times.
   */
  const handleRunSemanticIndexNow = async () => {
    ownsRun.current = true;
    setSemanticIndexRunning(true);
    setSemanticIndexStopping(false);
    setSemanticIndexRun(null);
    setSemanticIndexProgress(null);
    try {
      const summary = await invoke('semantic_index_run_now');
      setSemanticIndexRun(summary);
      await refreshSemanticStatus();
      return summary;
    } catch (err) {
      console.warn('Manual semantic indexing run failed:', err);
      setSemanticIndexRun({ started: false, skipped_reason: String(err) });
      return null;
    } finally {
      // Cleared before the flags, so the status read inside
      // `refreshSemanticStatus` above — which ran while this hook still owned
      // the run — is not the one that decides them.
      ownsRun.current = false;
      setSemanticIndexRunning(false);
      setSemanticIndexStopping(false);
      setSemanticIndexProgress(null);
    }
  };

  /**
   * Ask the running pass to stop. It checks between chunks, so this returns
   * long before the run does; the button stays in its "stopping" state until
   * the run reports what did land — through its own promise when this dialog
   * started it, or through the status poll when it did not.
   */
  const handleStopSemanticIndex = async () => {
    setSemanticIndexStopping(true);
    try {
      await invoke('semantic_index_stop_now');
    } catch (err) {
      console.warn('Failed to stop the semantic indexing run:', err);
      setSemanticIndexStopping(false);
    }
  };

  /**
   * The CLIP counterpart. Encoding an image costs far more than encoding a
   * line of text, so this run is the one most likely to last long enough for
   * somebody to walk away from — which is why it reports progress and stops on
   * request just like the MiniLM one.
   */
  const handleRunClipIndexNow = async () => {
    ownsClipRun.current = true;
    setClipIndexRunning(true);
    setClipIndexStopping(false);
    setClipIndexRun(null);
    setClipIndexProgress(null);
    try {
      const summary = await invoke('clip_index_run_now');
      setClipIndexRun(summary);
      await refreshSemanticStatus();
      return summary;
    } catch (err) {
      console.warn('Manual CLIP indexing run failed:', err);
      setClipIndexRun({ started: false, skipped_reason: String(err) });
      return null;
    } finally {
      ownsClipRun.current = false;
      setClipIndexRunning(false);
      setClipIndexStopping(false);
      setClipIndexProgress(null);
    }
  };

  const handleStopClipIndex = async () => {
    setClipIndexStopping(true);
    try {
      await invoke('clip_index_stop_now');
    } catch (err) {
      console.warn('Failed to stop the CLIP indexing run:', err);
      setClipIndexStopping(false);
    }
  };

  const refreshClipBackfill = async () => {
    try {
      setClipBackfill(await getClipBackfillOffer());
    } catch (err) {
      console.warn('Failed to read the CLIP backfill offer:', err);
    }
  };

  const handleClipBackfillDecision = async (decision) => {
    setClipBackfillBusy(true);
    try {
      setClipBackfill(await setClipBackfillDecision(decision));
    } catch (err) {
      console.warn('Failed to record the CLIP backfill decision:', err);
    } finally {
      setClipBackfillBusy(false);
    }
  };

  useEffect(() => {
    refreshClipBackfill();
  }, []);

  useEffect(() => {
    refreshMlOcrStatus();
    const timer = window.setInterval(refreshMlOcrStatus, 5000);
    return () => window.clearInterval(timer);
  }, []);

  const handleDownloadRustOcrModel = async () => {
    setRustOcrModelDownloading(true);
    try {
      const status = await invoke('download_rust_ocr_model');
      setRustOcrModelStatus(status);
      await refreshMlOcrStatus();
    } catch (err) {
      console.error('Failed to download Rust OCR model:', err);
    } finally {
      setRustOcrModelDownloading(false);
    }
  };

  const handleToggle = async (key) => {
    const newConfig = { ...config, [key]: !config[key] };
    const saved = await saveConfig(newConfig);
    if (!saved) return;
    if (key === 'cpu_limit_enabled') setCpuChanged(true);
    if (key === 'use_dml') setDmlChanged(true);
    if (key === 'use_onnx') setOnnxChanged(true);
    if (key === 'clustering_allow_full_low_memory') {
      await syncOcrConfigToMonitor(newConfig);
    }
    if (key === 'rust_ocr_dml_beta') {
      try {
        await withAuth(
          () => invoke('restart_ml_ocr_worker'),
          { autoPrompt: true },
        );
      } catch (err) {
        console.warn('Failed to restart Rust ML OCR worker:', err);
      }
      await refreshMlOcrStatus();
    }
  };

  const handleRestartMlOcr = async () => {
    try {
      await withAuth(
        () => invoke('restart_ml_ocr_worker'),
        { autoPrompt: true },
      );
    } finally {
      await refreshMlOcrStatus();
    }
  };

  const handleCpuPercentChange = async (value) => {
    setCpuDropdownOpen(false);
    await saveConfig({ ...config, cpu_limit_percent: value });
    setCpuChanged(true);
  };

  const handleOcrTimeoutDraftChange = (value) => {
    setConfig({ ...config, ocr_timeout_secs: value });
  };

  const handleOcrTimeoutChange = async (value) => {
    const parsed = Number.parseInt(value, 10);
    const next = Number.isFinite(parsed) ? Math.min(600, Math.max(30, parsed)) : 120;
    const newConfig = { ...config, ocr_timeout_secs: next };
    await saveConfig(newConfig);
    await syncOcrConfigToMonitor(newConfig);
  };

  const handleGpuChange = async (deviceId) => {
    setGpuDropdownOpen(false);
    await saveConfig({ ...config, dml_device_id: deviceId });
    setDmlChanged(true);
  };

  const handleClusteringIntervalChange = async (interval) => {
    setClusteringDropdownOpen(false);
    const newConfig = { ...config, clustering_interval: interval };
    await saveConfig(newConfig);
    try {
      await withAuth(() => invoke('monitor_set_clustering_interval', { interval }), { autoPrompt: true });
    } catch {
      // Best effort; persisted config will be applied on the next monitor refresh.
    }
  };

  const handleManualVacuum = async () => {
    setVacuumMessage('');
    setVacuumRunning(true);
    try {
      const result = await withAuth(() => invoke('storage_run_manual_vacuum'), { autoPrompt: true });
      if (result?.already_running) {
        setVacuumMessage(t('settings.advanced.vacuum.already_running', '已有数据库优化任务正在执行，请稍候。'));
      } else {
        setVacuumMessage(t('settings.advanced.vacuum.success', '数据库优化已完成。'));
      }
    } catch (err) {
      const msg = err?.message || err?.toString() || t('settings.advanced.vacuum.error', '数据库优化失败');
      setVacuumMessage(t('settings.advanced.vacuum.error_with_detail', '数据库优化失败：{{error}}', { error: msg }));
    } finally {
      await refreshVacuumRunningStatus();
    }
  };

  const selectedGpu = config ? (gpus.find((gpu) => gpu.id === config.dml_device_id) || gpus[0]) : null;

  return {
    config,
    loading,
    cpuDropdownOpen,
    gpuDropdownOpen,
    clusteringDropdownOpen,
    cpuChanged,
    dmlChanged,
    onnxChanged,
    gpus,
    gpuLoading,
    vacuumRunning,
    vacuumMessage,
    selectedGpu,
    mlOcrStatus,
    mlOcrStatusLoading,
    rustOcrModelStatus,
    rustOcrModelDownloading,
    semanticStatus,
    semanticStatusLoading,
    semanticIndexRunning,
    semanticIndexRun,
    clipIndexRunning,
    clipIndexRun,
    clipIndexStopping,
    clipIndexProgress,
    clipBackfill,
    clipBackfillBusy,
    handleClipBackfillDecision,
    semanticIndexProgress,
    semanticIndexStopping,
    setCpuDropdownOpen,
    setGpuDropdownOpen,
    setClusteringDropdownOpen,
    clearCpuChanged: () => setCpuChanged(false),
    clearDmlChanged: () => setDmlChanged(false),
    clearOnnxChanged: () => setOnnxChanged(false),
    handleToggle,
    handleCpuPercentChange,
    handleOcrTimeoutDraftChange,
    handleOcrTimeoutChange,
    handleGpuChange,
    handleClusteringIntervalChange,
    handleManualVacuum,
    handleRestartMlOcr,
    handleDownloadRustOcrModel,
    handleToggleRustSemanticIndex,
    handleRunSemanticIndexNow,
    handleStopSemanticIndex,
    handleToggleRustClipIndex,
    handleRunClipIndexNow,
    handleStopClipIndex,
    refreshSemanticStatus,
  };
}
