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
  const [semanticIndexPhase, setSemanticIndexPhase] = useState('idle');
  const [semanticIndexRetryAt, setSemanticIndexRetryAt] = useState(null);
  const [semanticIndexRun, setSemanticIndexRun] = useState(null);
  // The CLIP image index has its own run and progress event. The two passes can
  // run independently, so one status line for both would report whichever
  // finished last.
  const [clipIndexRunning, setClipIndexRunning] = useState(false);
  const [clipIndexPhase, setClipIndexPhase] = useState('idle');
  const [clipIndexRetryAt, setClipIndexRetryAt] = useState(null);
  const [clipIndexRun, setClipIndexRun] = useState(null);
  const [clipIndexStopping, setClipIndexStopping] = useState(false);
  const [clipIndexProgress, setClipIndexProgress] = useState(null);
  const [clipAnnRetrying, setClipAnnRetrying] = useState(false);
  // Whether a backfill of everything the step-7 migration could not deliver has
  // been offered, and what the user said. The dialog asks once; this is where
  // the answer stays changeable, which is the whole reason declining is safe to
  // record durably.
  const [clipBackfill, setClipBackfill] = useState(null);
  const [clipBackfillBusy, setClipBackfillBusy] = useState(false);
  const [semanticIndexProgress, setSemanticIndexProgress] = useState(null);
  const [semanticIndexStopping, setSemanticIndexStopping] = useState(false);
  const [backgroundProcessingEnabled, setBackgroundProcessingEnabled] = useState(true);
  const [backgroundSchedulerStatus, setBackgroundSchedulerStatus] = useState(null);
  const [backgroundProcessingSaving, setBackgroundProcessingSaving] = useState(false);
  const mlOcrStatusRequestRef = useRef(null);

  // These refs prevent a status poll from overwriting state while the current
  // settings panel still owns the command promise. Once the command hands the
  // work to the scheduler, polling becomes the source of truth again.
  const ownsRun = useRef(false);
  const ownsClipRun = useRef(false);

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
      setVacuumRunning(Boolean(await invoke('storage_is_startup_vacuum_in_progress')));
    } catch {
      setVacuumRunning(false);
    }
  };

  useEffect(() => {
    loadConfig();
  }, []);

  const schedulerIndexPhase = (status, kind) => {
    const tasks = Array.isArray(status?.tasks) ? status.tasks : null;
    const running = Boolean(status?.running_manual && status.running_task === kind);
    const row = tasks?.find((item) => item.task_kind === kind);
    // A transient IPC/database read can return no row (the backend currently
    // falls back to an empty task list when that read fails). Do not turn a
    // known queued/retry state into idle on that incomplete snapshot; the
    // durable row will reconcile on the next poll.
    if (!row && !running) return null;
    if (running) return { phase: 'running', retryAt: null };
    if (row?.manual_pending && row.status === 'retry_wait') {
      return { phase: 'retry_wait', retryAt: row.next_attempt_at_ms || null };
    }
    if (row?.manual_pending) return { phase: 'queued', retryAt: null };
    if (row?.status === 'failed') return { phase: 'failed', retryAt: null };
    return { phase: 'idle', retryAt: null };
  };

  const refreshBackgroundSchedulerStatus = async () => {
    try {
      const [enabled, status] = await Promise.all([
        invoke('credential_get_background_processing_enabled'),
        invoke('background_scheduler_status'),
      ]);
      setBackgroundProcessingEnabled(Boolean(enabled));
      setBackgroundSchedulerStatus(status);
      if (status) {
        const applyIndexState = ({
          kind,
          ownerRef,
          setPhase,
          setRetryAt,
          setRunning,
          setStopping,
        }) => {
          if (ownerRef.current) return;
          const resolved = schedulerIndexPhase(status, kind);
          if (!resolved) return;
          const { phase, retryAt } = resolved;
          setPhase(phase);
          setRetryAt(retryAt);
          // Keep the old boolean for callers that use it as a compact
          // "active/queued" indicator. `retry_wait` and `failed` are
          // deliberately not active: they are waiting states, not runs.
          setRunning(phase === 'running' || phase === 'queued');
          if (phase !== 'running' && phase !== 'queued') setStopping(false);
        };
        applyIndexState({
          kind: 'semantic_index',
          ownerRef: ownsRun,
          setPhase: setSemanticIndexPhase,
          setRetryAt: setSemanticIndexRetryAt,
          setRunning: setSemanticIndexRunning,
          setStopping: setSemanticIndexStopping,
        });
        applyIndexState({
          kind: 'clip_index',
          ownerRef: ownsClipRun,
          setPhase: setClipIndexPhase,
          setRetryAt: setClipIndexRetryAt,
          setRunning: setClipIndexRunning,
          setStopping: setClipIndexStopping,
        });
      }
    } catch (err) {
      console.warn('Failed to read background scheduler status:', err);
    }
  };

  useEffect(() => {
    let cancelled = false;
    let timer = null;
    const poll = async () => {
      await refreshBackgroundSchedulerStatus();
      if (!cancelled) timer = window.setTimeout(poll, 3000);
    };
    poll();
    return () => {
      cancelled = true;
      if (timer) window.clearTimeout(timer);
    };
  }, []);

  const handleBackgroundProcessingChange = async (enabled) => {
    setBackgroundProcessingSaving(true);
    try {
      await withAuth(
        () => invoke('credential_set_background_processing_enabled', { enabled }),
        { autoPrompt: true },
      );
      setBackgroundProcessingEnabled(enabled);
      await refreshBackgroundSchedulerStatus();
    } catch (err) {
      console.warn('Failed to update background processing:', err);
    } finally {
      setBackgroundProcessingSaving(false);
    }
  };

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

  const refreshMlOcrStatus = () => {
    if (mlOcrStatusRequestRef.current) return mlOcrStatusRequestRef.current;

    setMlOcrStatusLoading(true);
    const request = (async () => {
      try {
        setMlOcrStatus(await invoke('get_ml_ocr_status'));
      } catch (err) {
        console.warn('Failed to read Rust ML OCR status:', err);
      } finally {
        setMlOcrStatusLoading(false);
      }
    })();
    mlOcrStatusRequestRef.current = request.finally(() => {
      mlOcrStatusRequestRef.current = null;
    });
    return mlOcrStatusRequestRef.current;
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

  const readSemanticStatus = async ({ quiet = false, refreshDiagnostics = false } = {}) => {
    if (!quiet) setSemanticStatusLoading(true);
    try {
      const status = await invoke('get_ml_semantic_status', { refreshDiagnostics });
      setSemanticStatus(status);
      // The diagnostic endpoint can see a run that was started before this
      // panel mounted. It is a useful positive signal, but an inactive value
      // must not clear `retry_wait`/`queued`: those phases come from the
      // durable scheduler and are intentionally independent of the worker's
      // short-lived active flag.
      if (!ownsRun.current && status?.backend?.index_run_active) {
        setSemanticIndexPhase('running');
        setSemanticIndexRunning(true);
      }
      if (!ownsClipRun.current && status?.clip_backend?.index_run_active) {
        setClipIndexPhase('running');
        setClipIndexRunning(true);
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
  const refreshSemanticStatus = async () => {
    const [status] = await Promise.all([
      readSemanticStatus({ refreshDiagnostics: true }),
      refreshClipBackfill(true),
      refreshBackgroundSchedulerStatus(),
    ]);
    return status;
  };

  useEffect(() => {
    readSemanticStatus();
  }, []);

  /**
   * Notice the end of either run this dialog does not own. Two seconds is far
   * shorter than the time it takes to wonder why a button is still disabled.
   * The scheduler poll is the authority for clearing the phase; this read is
   * retained as a compatibility fallback for runs that predate its row.
   */
  useEffect(() => {
    const watchingSemantic = semanticIndexPhase === 'running' && !ownsRun.current;
    const watchingClip = clipIndexPhase === 'running' && !ownsClipRun.current;
    if (!watchingSemantic && !watchingClip) return undefined;
    const timer = window.setInterval(() => readSemanticStatus({ quiet: true }), 2000);
    return () => window.clearInterval(timer);
  }, [semanticIndexPhase, clipIndexPhase]);

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
    let queued = false;
    ownsRun.current = true;
    setSemanticIndexRunning(true);
    setSemanticIndexPhase('running');
    setSemanticIndexRetryAt(null);
    setSemanticIndexStopping(false);
    setSemanticIndexRun(null);
    setSemanticIndexProgress(null);
    try {
      const summary = await invoke('semantic_index_run_now');
      setSemanticIndexRun(summary);
      if (summary?.queued) {
        queued = true;
        ownsRun.current = false;
        setSemanticIndexPhase('queued');
        await refreshBackgroundSchedulerStatus();
        return summary;
      }
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
      if (!queued) {
        setSemanticIndexRunning(false);
        setSemanticIndexPhase('idle');
      }
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
    let queued = false;
    ownsClipRun.current = true;
    setClipIndexRunning(true);
    setClipIndexPhase('running');
    setClipIndexRetryAt(null);
    setClipIndexStopping(false);
    setClipIndexRun(null);
    setClipIndexProgress(null);
    try {
      const summary = await invoke('clip_index_run_now');
      setClipIndexRun(summary);
      if (summary?.queued) {
        queued = true;
        ownsClipRun.current = false;
        setClipIndexPhase('queued');
        await refreshBackgroundSchedulerStatus();
        return summary;
      }
      await refreshSemanticStatus();
      return summary;
    } catch (err) {
      console.warn('Manual CLIP indexing run failed:', err);
      setClipIndexRun({ started: false, skipped_reason: String(err) });
      return null;
    } finally {
      ownsClipRun.current = false;
      if (!queued) {
        setClipIndexRunning(false);
        setClipIndexPhase('idle');
      }
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

  const handleRetryClipAnn = async () => {
    setClipAnnRetrying(true);
    try {
      await withAuth(() => invoke('clip_ann_retry_now'), { autoPrompt: true });
    } catch (err) {
      console.warn('Manual ANN rebuild retry failed:', err);
    } finally {
      setClipAnnRetrying(false);
      await refreshSemanticStatus();
    }
  };

  const refreshClipBackfill = async (allowExpensive = false) => {
    try {
      setClipBackfill(await getClipBackfillOffer(allowExpensive));
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
    let cancelled = false;
    let timer = null;
    const poll = async () => {
      await refreshMlOcrStatus();
      if (!cancelled) timer = window.setTimeout(poll, 5000);
    };
    poll();
    return () => {
      cancelled = true;
      if (timer) window.clearTimeout(timer);
    };
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
    if (key === 'clustering_allow_full_low_memory') {
      await syncOcrConfigToMonitor(newConfig);
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
    semanticIndexPhase,
    semanticIndexRetryAt,
    semanticIndexRun,
    clipIndexRunning,
    clipIndexPhase,
    clipIndexRetryAt,
    clipIndexRun,
    clipIndexStopping,
    clipIndexProgress,
    clipAnnRetrying,
    clipBackfill,
    clipBackfillBusy,
    handleClipBackfillDecision,
    semanticIndexProgress,
    semanticIndexStopping,
    backgroundProcessingEnabled,
    backgroundSchedulerStatus,
    backgroundProcessingSaving,
    setCpuDropdownOpen,
    setGpuDropdownOpen,
    setClusteringDropdownOpen,
    clearCpuChanged: () => setCpuChanged(false),
    clearDmlChanged: () => setDmlChanged(false),
    handleToggle,
    handleCpuPercentChange,
    handleOcrTimeoutDraftChange,
    handleOcrTimeoutChange,
    handleGpuChange,
    handleClusteringIntervalChange,
    handleManualVacuum,
    handleRestartMlOcr,
    handleDownloadRustOcrModel,
    handleRunSemanticIndexNow,
    handleStopSemanticIndex,
    handleRunClipIndexNow,
    handleStopClipIndex,
    handleRetryClipAnn,
    refreshSemanticStatus,
    refreshBackgroundSchedulerStatus,
    handleBackgroundProcessingChange,
  };
}
