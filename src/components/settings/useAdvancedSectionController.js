import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../lib/auth_api';
import { getClipBackfillOffer, setClipBackfillDecision } from '../../lib/semantic_api';
import { useAdvancedPreferences } from './advanced/useAdvancedPreferences';
import { useSettingsActivity } from './SettingsActivityContext';
import { notifySettingsChanged } from '../../lib/settings_api';
import { useBackgroundIndexProgress } from '../../hooks/useBackgroundIndexProgress';
import { useIndexTaskController } from './hooks/useIndexTaskController';
import { usePolling } from '../../hooks/usePolling';
import { getThumbnailWarmupStatus, warmupThumbnails, cancelThumbnailWarmup } from '../../lib/monitor_api';

export function useAdvancedSectionController({ monitorStatus, t, active = true, diagnosticsActive = active, maintenanceActive = active }) {
  const preferences = useAdvancedPreferences({ monitorStatus, t, active, devicesActive: diagnosticsActive });
  const [vacuumRunning, setVacuumRunning] = useState(false);
  const [vacuumMessage, setVacuumMessage] = useState('');
  const [mlOcrStatus, setMlOcrStatus] = useState(null);
  const [mlOcrStatusLoading, setMlOcrStatusLoading] = useState(false);
  const [rustOcrModelStatus, setRustOcrModelStatus] = useState(null);
  const [rustOcrModelDownloading, setRustOcrModelDownloading] = useState(false);
  const [semanticStatus, setSemanticStatus] = useState(null);
  const [semanticStatusLoading, setSemanticStatusLoading] = useState(false);
  const { progress: backgroundIndex, refresh: refreshIndexProgress } = useBackgroundIndexProgress(active);
  const semanticIndex = useIndexTaskController('semantic', backgroundIndex.semantic, refreshIndexProgress, t);
  const clipIndex = useIndexTaskController('clip', backgroundIndex.clip, refreshIndexProgress, t);
  const [clipAnnRetrying, setClipAnnRetrying] = useState(false);
  // Whether a backfill of everything the step-7 migration could not deliver has
  // been offered, and what the user said. The dialog asks once; this is where
  // the answer stays changeable, which is the whole reason declining is safe to
  // record durably.
  const [clipBackfill, setClipBackfill] = useState(null);
  const [clipBackfillBusy, setClipBackfillBusy] = useState(false);
  const [backgroundProcessingEnabled, setBackgroundProcessingEnabled] = useState(null);
  const [backgroundSchedulerStatus, setBackgroundSchedulerStatus] = useState(null);
  const [backgroundProcessingSaving, setBackgroundProcessingSaving] = useState(false);
  const mlOcrStatusRequestRef = useRef(null);
  const [runtimeError, setRuntimeError] = useState('');
  const [thumbnailWarmup, setThumbnailWarmup] = useState(null);
  const [thumbnailWarmupBusy, setThumbnailWarmupBusy] = useState(false);
  const [thumbnailWarmupError, setThumbnailWarmupError] = useState('');
  const [thumbnailWarmupReadError, setThumbnailWarmupReadError] = useState(false);
  const thumbnailWarmupRequest = useRef(0);
  useSettingsActivity('runtime-operation', { busy: rustOcrModelDownloading || backgroundProcessingSaving });

  usePolling(async () => {
    const request = ++thumbnailWarmupRequest.current;
    try {
      const status = await getThumbnailWarmupStatus();
      if (request !== thumbnailWarmupRequest.current) return;
      setThumbnailWarmup(status);
      setThumbnailWarmupReadError(false);
    } catch {
      if (request === thumbnailWarmupRequest.current) setThumbnailWarmupReadError(true);
    }
  }, { intervalMs: 1000, enabled: maintenanceActive && !thumbnailWarmupBusy });

  const handleThumbnailWarmup = async () => {
    if (thumbnailWarmupBusy) return;
    ++thumbnailWarmupRequest.current;
    setThumbnailWarmupBusy(true);
    setThumbnailWarmupError('');
    try {
      const status = thumbnailWarmup?.running
        ? await cancelThumbnailWarmup()
        : (await warmupThumbnails({ autoPrompt: true })).progress;
      setThumbnailWarmup(status);
    } catch (error) {
      setThumbnailWarmupError(t('settings.feedback.operationFailed', { error: String(error) }));
    } finally {
      setThumbnailWarmupBusy(false);
    }
  };

  const refreshVacuumRunningStatus = async () => {
    try {
      setVacuumRunning(Boolean(await invoke('storage_is_startup_vacuum_in_progress')));
    } catch {
      setVacuumRunning(false);
    }
  };

  const refreshBackgroundSchedulerStatus = async () => {
    try {
      const [enabled, status] = await Promise.all([
        invoke('credential_get_background_processing_enabled'),
        invoke('background_scheduler_status'),
      ]);
      setBackgroundProcessingEnabled(Boolean(enabled));
      setBackgroundSchedulerStatus(status);
    } catch (err) {
      console.warn('Failed to read background scheduler status:', err);
    }
  };

  useEffect(() => {
    if (!active) return undefined;
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
  }, [active]);

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
      setRuntimeError(t('settings.feedback.saveFailed', { error: String(err) }));
    } finally {
      setBackgroundProcessingSaving(false);
    }
  };

  useEffect(() => {
    if (active) refreshVacuumRunningStatus();
  }, [active]);

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
      setRuntimeError('');
      setRustOcrModelStatus(await invoke('get_rust_ocr_model_status'));
    } catch (err) {
      setRuntimeError(t('settings.feedback.readFailed'));
    }
  };

  useEffect(() => {
    if (diagnosticsActive) refreshRustOcrModelStatus();
  }, [diagnosticsActive]);

  const readSemanticStatus = async ({ quiet = false, refreshDiagnostics = false } = {}) => {
    if (!quiet) setSemanticStatusLoading(true);
    try {
      const status = await invoke('get_ml_semantic_status', { refreshDiagnostics });
      setSemanticStatus(status);
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
      refreshIndexProgress({ fresh: true }),
    ]);
    return status;
  };

  useEffect(() => {
    if (active) readSemanticStatus();
  }, [active]);

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
    if (active) refreshClipBackfill();
  }, [active]);

  useEffect(() => {
    if (!diagnosticsActive) return undefined;
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
  }, [diagnosticsActive]);

  const handleDownloadRustOcrModel = async () => {
    setRustOcrModelDownloading(true);
    try {
      setRuntimeError('');
      const status = await withAuth(() => invoke('download_rust_ocr_model'), { autoPrompt: true });
      await notifySettingsChanged(['models']);
      setRustOcrModelStatus(status);
      await refreshMlOcrStatus();
    } catch (err) {
      setRuntimeError(t('settings.feedback.operationFailed', { error: String(err) }));
    } finally {
      setRustOcrModelDownloading(false);
    }
  };

  const handleRestartMlOcr = async () => {
    try {
      setRuntimeError('');
      await withAuth(
        () => invoke('restart_ml_ocr_worker'),
        { autoPrompt: true },
      );
    } catch (error) {
      setRuntimeError(t('settings.feedback.operationFailed', { error: String(error) }));
    } finally {
      await refreshMlOcrStatus();
    }
  };

  const handleManualVacuum = async () => {
    setVacuumMessage('');
    setVacuumRunning(true);
    try {
      const result = await withAuth(() => invoke('storage_run_manual_vacuum'), { autoPrompt: true });
      if (result?.already_running) {
        setVacuumMessage(t('settings.advanced.vacuum.already_running'));
      } else {
        setVacuumMessage(t('settings.advanced.vacuum.success'));
      }
    } catch (err) {
      const msg = err?.message || err?.toString() || t('settings.advanced.vacuum.error');
      setVacuumMessage(t('settings.advanced.vacuum.error_with_detail', { error: msg }));
    } finally {
      await refreshVacuumRunningStatus();
    }
  };

  return {
    preferences,
    ocr: {
      status: mlOcrStatus,
      statusLoading: mlOcrStatusLoading,
      modelStatus: rustOcrModelStatus,
      modelDownloading: rustOcrModelDownloading,
      refreshModel: refreshRustOcrModelStatus,
      restart: handleRestartMlOcr,
      downloadModel: handleDownloadRustOcrModel,
      error: runtimeError,
    },
    indexes: {
      status: semanticStatus,
      statusLoading: semanticStatusLoading,
      refresh: refreshSemanticStatus,
      semantic: semanticIndex,
      clip: clipIndex,
      ann: { retrying: clipAnnRetrying, retry: handleRetryClipAnn },
      backfill: { offer: clipBackfill, busy: clipBackfillBusy, setDecision: handleClipBackfillDecision },
    },
    scheduler: {
      enabled: backgroundProcessingEnabled,
      status: backgroundSchedulerStatus,
      saving: backgroundProcessingSaving,
      refresh: refreshBackgroundSchedulerStatus,
      setEnabled: handleBackgroundProcessingChange,
    },
    thumbnails: {
      status: thumbnailWarmup,
      busy: thumbnailWarmupBusy,
      error: thumbnailWarmupError || (thumbnailWarmupReadError ? t('settings.feedback.readFailed') : ''),
      toggle: handleThumbnailWarmup,
    },
    vacuum: { running: vacuumRunning, message: vacuumMessage, run: handleManualVacuum },
    error: runtimeError,
  };
}
