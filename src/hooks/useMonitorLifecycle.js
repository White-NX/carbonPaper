import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../lib/auth_api';
import { useTauriEventListener } from './useTauriEventListener';

export function useMonitorLifecycle({
  pythonVersion,
  depsNeedUpdate,
  depsSyncing,
  depsCheckDone,
  modelsNeedDownload,
  powerSavingSuppressed,
  formatErrorDetails,
  reportBackendError,
  resetBackendErrorDedupe,
  t,
}) {
  const [autoStartMonitor, setAutoStartMonitorState] = useState(() => {
    if (typeof window === 'undefined') return true;
    const saved = localStorage.getItem('autoStartMonitor');
    return saved === null ? true : saved === 'true';
  });
  const [autoStartSuppressed, setAutoStartSuppressed] = useState(false);
  const [backendStatus, setBackendStatus] = useState('unknown');
  const [monitorPaused, setMonitorPaused] = useState(false);
  const [backendError, setBackendError] = useState('');
  const backendStatusRef = useRef('unknown');
  const backendStartAtRef = useRef(null);

  useEffect(() => {
    backendStatusRef.current = backendStatus;
  }, [backendStatus]);

  useEffect(() => {
    localStorage.setItem('autoStartMonitor', autoStartMonitor ? 'true' : 'false');
  }, [autoStartMonitor]);

  const setAutoStartMonitor = useCallback((next) => {
    setAutoStartMonitorState(next);
    if (next) {
      setAutoStartSuppressed(false);
    }
  }, []);

  const handleManualStartMonitor = useCallback(() => {
    setAutoStartSuppressed(false);
  }, []);

  const handleManualStopMonitor = useCallback(() => {
    setAutoStartSuppressed(true);
  }, []);

  const handleStartBackend = useCallback(async () => {
    setAutoStartSuppressed(false);
    setBackendError('');
    setBackendStatus('waiting');
    backendStatusRef.current = 'waiting';
    backendStartAtRef.current = Date.now();
    try {
      await invoke('start_monitor');
    } catch (err) {
      setBackendStatus('offline');
      backendStatusRef.current = 'offline';
      const message = err?.message || t('settings.general.monitor.errors.startFailedFallback');
      const details = formatErrorDetails(err);
      setBackendError(message);
      setAutoStartSuppressed(true);
      reportBackendError(t('settings.general.monitor.errors.startFailedTitle'), message, details);
    }
  }, [formatErrorDetails, reportBackendError, t]);

  const handlePauseMonitor = useCallback(async () => {
    try {
      await withAuth(() => invoke('pause_monitor'), { autoPrompt: true });
      setMonitorPaused(true);
    } catch (err) {
      console.warn('Failed to pause monitor:', err);
    }
  }, []);

  const handleResumeMonitor = useCallback(async () => {
    try {
      await withAuth(() => invoke('resume_monitor'), { autoPrompt: true });
      setMonitorPaused(false);
    } catch (err) {
      console.warn('Failed to resume monitor:', err);
    }
  }, []);

  const checkBackendStatus = useCallback(async () => {
    const t0 = performance.now();
    try {
      const resString = await invoke('get_monitor_status');
      const elapsed = performance.now() - t0;
      if (elapsed > 5000) {
        console.warn(`[DIAG:STATUS] get_monitor_status took ${elapsed.toFixed(0)}ms`);
      }
      let res = null;
      try {
        res = JSON.parse(resString);
      } catch {
        res = null;
      }

      if (res?.stopped) {
        setBackendStatus('offline');
        backendStatusRef.current = 'offline';
        setMonitorPaused(false);
        setBackendError('');
        resetBackendErrorDedupe();
        backendStartAtRef.current = null;
        return;
      }

      setBackendStatus('online');
      backendStatusRef.current = 'online';
      setMonitorPaused(!!res?.paused);
      setBackendError('');
      resetBackendErrorDedupe();
      backendStartAtRef.current = null;
    } catch (err) {
      const elapsed = performance.now() - t0;
      if (elapsed > 5000) {
        console.warn(`[DIAG:STATUS] get_monitor_status FAILED after ${elapsed.toFixed(0)}ms:`, err);
      }
      if (backendStatusRef.current === 'waiting') {
        const startAt = backendStartAtRef.current;
        if (startAt && Date.now() - startAt < 15000) {
          return;
        }
      }
      setBackendStatus('offline');
      backendStatusRef.current = 'offline';
      const message = err?.message || t('settings.general.monitor.errors.offlineFallback');
      const details = formatErrorDetails(err);
      setBackendError(message);
      reportBackendError(t('settings.general.monitor.errors.unavailableTitle'), message, details);
    }
  }, [formatErrorDetails, reportBackendError, resetBackendErrorDedupe, t]);

  useEffect(() => {
    checkBackendStatus();
    const interval = setInterval(checkBackendStatus, 3000);
    return () => clearInterval(interval);
  }, [checkBackendStatus]);

  useTauriEventListener('monitor-exited', (event) => {
    const payload = event?.payload || {};
    const code = payload.code || 'unknown';
    const errMsg = payload.error ? `; ${payload.error}` : '';
    const recovery = payload.recovery || {};
    const recoveryMsg = recovery.policy === 'manual_restart'
      ? t('settings.general.monitor.errors.manualRestartRecovery')
      : '';
    const message = t('settings.general.monitor.errors.exitedMessage', {
      code,
      error: errMsg,
      recovery: recoveryMsg,
    });
    const details = formatErrorDetails(payload);
    setBackendStatus('offline');
    backendStatusRef.current = 'offline';
    setBackendError(message);
    reportBackendError(t('settings.general.monitor.errors.exitedTitle'), message, details);
  }, [formatErrorDetails, reportBackendError, t]);

  useTauriEventListener('monitor-stopped', () => {
    setBackendStatus('offline');
    backendStatusRef.current = 'offline';
    setMonitorPaused(false);
    setBackendError('');
    resetBackendErrorDedupe();
    backendStartAtRef.current = null;
  }, [resetBackendErrorDedupe]);

  useEffect(() => {
    if (!autoStartMonitor) return;
    if (autoStartSuppressed) return;
    if (powerSavingSuppressed) return;
    if (!pythonVersion) return;
    if (!depsCheckDone) return;
    if (depsNeedUpdate || depsSyncing) return;
    if (modelsNeedDownload) return;
    if (backendStatus === 'offline' && backendStatusRef.current !== 'waiting') {
      handleStartBackend();
    }
  }, [
    autoStartMonitor,
    autoStartSuppressed,
    backendStatus,
    depsCheckDone,
    depsNeedUpdate,
    depsSyncing,
    handleStartBackend,
    modelsNeedDownload,
    powerSavingSuppressed,
    pythonVersion,
  ]);

  return {
    autoStartMonitor,
    setAutoStartMonitor,
    handleManualStartMonitor,
    handleManualStopMonitor,
    backendStatus,
    monitorPaused,
    backendError,
    handleStartBackend,
    handlePauseMonitor,
    handleResumeMonitor,
  };
}
