import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { runMonitorAction } from '../lib/settings_api';
import { usePolling } from './usePolling';
import { useTauriEventListener } from './useTauriEventListener';

// Rust owns automatic admission, manual-stop intent and recovery. This hook
// observes the existing runtime and forwards explicit user actions.
export function useMonitorLifecycle({
  powerSavingSuppressed,
  formatErrorDetails,
  reportBackendError,
  resetBackendErrorDedupe,
  t,
}) {
  const [backendStatus, setBackendStatus] = useState('unknown');
  const [monitorPaused, setMonitorPaused] = useState(false);
  const [backendError, setBackendError] = useState('');
  const [powerBlocked, setPowerBlocked] = useState(Boolean(powerSavingSuppressed));
  const latestRevision = useRef(-1);
  const powerEventVersion = useRef(0);
  const alive = useRef(false);

  useEffect(() => {
    alive.current = true;
    return () => { alive.current = false; };
  }, []);

  useEffect(() => {
    powerEventVersion.current += 1;
    setPowerBlocked(Boolean(powerSavingSuppressed));
  }, [powerSavingSuppressed]);

  const reportError = useCallback((error) => {
    const message = error === 'CAPTURE_RESTART_LIMIT'
      ? t('settings.general.monitor.errors.restartLimit')
      : typeof error === 'string' ? error : error?.message || t('settings.general.monitor.errors.startFailedFallback');
    setBackendError(message);
    reportBackendError(t('settings.general.monitor.errors.startFailedTitle'), message, formatErrorDetails(error));
  }, [formatErrorDetails, reportBackendError, t]);

  const acceptStatus = useCallback((status, powerVersion = powerEventVersion.current) => {
    if (!alive.current || !status || typeof status.stopped !== 'boolean') return;
    // An event may arrive before the response to an older status query.
    if (Number.isFinite(status.revision)) {
      if (status.revision < latestRevision.current) return;
      latestRevision.current = status.revision;
    }
    setBackendStatus(status.starting ? 'waiting' : status.stopped ? 'offline' : 'online');
    setMonitorPaused(!status.stopped && Boolean(status.paused));
    if (powerVersion === powerEventVersion.current && typeof status.power_saving_active === 'boolean') {
      setPowerBlocked(status.power_saving_active);
    }
    if (status.error) reportError(status.error);
    else {
      setBackendError('');
      resetBackendErrorDedupe();
    }
  }, [reportError, resetBackendErrorDedupe]);

  const checkBackendStatus = useCallback(async () => {
    const powerVersion = powerEventVersion.current;
    try {
      const response = await invoke('get_monitor_status');
      const status = typeof response === 'string' ? JSON.parse(response) : response;
      acceptStatus(status, powerVersion);
    } catch (error) {
      if (!alive.current) return;
      setBackendStatus('offline');
      const message = error?.message || t('settings.general.monitor.errors.offlineFallback');
      setBackendError(message);
      reportBackendError(t('settings.general.monitor.errors.unavailableTitle'), message, formatErrorDetails(error));
    }
  }, [acceptStatus, formatErrorDetails, reportBackendError, t]);

  usePolling(checkBackendStatus, { intervalMs: 3000 });
  useTauriEventListener('monitor-status-changed', ({ payload }) => acceptStatus(payload), [acceptStatus]);
  useTauriEventListener('monitor-stopped', checkBackendStatus, [checkBackendStatus]);

  const handleStartBackend = useCallback(async () => {
    setBackendStatus('waiting');
    setBackendError('');
    try {
      await runMonitorAction('start');
      await checkBackendStatus();
    } catch (error) {
      setBackendStatus('offline');
      if ((typeof error === 'string' ? error : error?.message) === 'POWER_SAVING_ACTIVE') {
        powerEventVersion.current += 1;
        setPowerBlocked(true);
        return;
      }
      reportError(error);
    }
  }, [checkBackendStatus, reportError]);

  const handlePauseMonitor = useCallback(async () => {
    try {
      await runMonitorAction('pause');
      await checkBackendStatus();
    } catch (error) {
      console.warn('Failed to pause monitor:', error);
    }
  }, [checkBackendStatus]);

  const handleResumeMonitor = useCallback(async () => {
    try {
      await runMonitorAction('resume');
      await checkBackendStatus();
    } catch (error) {
      console.warn('Failed to resume monitor:', error);
    }
  }, [checkBackendStatus]);

  return {
    backendStatus,
    monitorPowerSavingSuppressed: powerBlocked,
    monitorPaused,
    backendError,
    handleStartBackend,
    handlePauseMonitor,
    handleResumeMonitor,
  };
}
