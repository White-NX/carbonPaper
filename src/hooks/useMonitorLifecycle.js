import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../lib/auth_api';
import { useTauriEventListener } from './useTauriEventListener';

// An unexpected stop is a capture loop that ended without being asked to. The
// auto-start effect restarts it, and a failure that recurs on every start would
// otherwise be restarted indefinitely.
const MAX_UNEXPECTED_STOPS = 3;
const UNEXPECTED_STOP_WINDOW_MS = 10 * 60 * 1000;

export function useMonitorLifecycle({
  modelsCheckDone,
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
  const autoStartSuppressedRef = useRef(false);
  const maintenanceSuppressedRef = useRef(null);
  const runtimeActionRef = useRef(false);
  const [backendStatus, setBackendStatus] = useState('unknown');
  const [monitorPaused, setMonitorPaused] = useState(false);
  const [backendError, setBackendError] = useState('');
  const backendStatusRef = useRef('unknown');
  const backendStartAtRef = useRef(null);
  const unexpectedStopsRef = useRef([]);

  useEffect(() => {
    backendStatusRef.current = backendStatus;
  }, [backendStatus]);

  useEffect(() => {
    let cancelled = false;
    invoke('get_monitor_autostart')
      .then((enabled) => {
        if (!cancelled) setAutoStartMonitorState(Boolean(enabled));
      })
      .catch(() => {
        // Keep the legacy local preference as a compatibility fallback.
      });
    return () => { cancelled = true; };
  }, []);

  useEffect(() => {
    localStorage.setItem('autoStartMonitor', autoStartMonitor ? 'true' : 'false');
  }, [autoStartMonitor]);

  const setAutoStartMonitor = useCallback(async (next) => {
    const previous = autoStartMonitor;
    setAutoStartMonitorState(next);
    try {
      await withAuth(
        () => invoke('set_monitor_autostart', { enabled: next }),
        { autoPrompt: true },
      );
      if (next) setAutoStartSuppressed(false);
    } catch (err) {
      setAutoStartMonitorState(previous);
      console.warn('Failed to update monitor auto-start policy:', err);
    }
  }, [autoStartMonitor]);

  const handleManualStartMonitor = useCallback(() => {
    autoStartSuppressedRef.current = false;
    setAutoStartSuppressed(false);
  }, []);

  const handleManualStopMonitor = useCallback(() => {
    autoStartSuppressedRef.current = true;
    setAutoStartSuppressed(true);
  }, []);

  const handleStartBackend = useCallback(async () => {
    autoStartSuppressedRef.current = false;
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
      await invoke('pause_monitor');
      setMonitorPaused(true);
    } catch (err) {
      console.warn('Failed to pause monitor:', err);
    }
  }, []);

  const handleResumeMonitor = useCallback(async () => {
    try {
      await invoke('resume_monitor');
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

  // One owner for manual and maintenance commands, including requests from the
  // settings window. Suppress auto-start synchronously before stopping capture.
  const handleSettingsMonitorAction = useCallback(async (action) => {
    if (runtimeActionRef.current) throw new Error('MONITOR_ACTION_BUSY');
    runtimeActionRef.current = true;
    const previous = autoStartSuppressedRef.current;
    try {
      if (action === 'pause' || action === 'resume') {
        await invoke(action === 'pause' ? 'pause_monitor' : 'resume_monitor');
      } else {
        if (!['start', 'stop', 'restart', 'maintenance-stop', 'maintenance-start'].includes(action)) throw new Error('INVALID_MONITOR_ACTION');
        if (action === 'maintenance-stop' && maintenanceSuppressedRef.current === null) maintenanceSuppressedRef.current = previous;
        autoStartSuppressedRef.current = true;
        setAutoStartSuppressed(true);
        if (['stop', 'restart', 'maintenance-stop'].includes(action)) await invoke('stop_monitor');
        if (['start', 'restart', 'maintenance-start'].includes(action)) {
          if (powerSavingSuppressed) throw new Error(t('settings.general.monitor.power_saving_blocked'));
          setBackendStatus('waiting');
          backendStatusRef.current = 'waiting';
          await invoke('start_monitor');
          await checkBackendStatus();
          const suppressed = action === 'maintenance-start' ? Boolean(maintenanceSuppressedRef.current) : false;
          maintenanceSuppressedRef.current = null;
          autoStartSuppressedRef.current = suppressed;
          setAutoStartSuppressed(suppressed);
        }
      }
      await checkBackendStatus();
    } finally { runtimeActionRef.current = false; }
  }, [checkBackendStatus, powerSavingSuppressed, t]);

  useTauriEventListener('settings-preferences-changed', ({ payload }) => {
    if (payload?.includes('autostart')) {
      invoke('get_monitor_autostart').then((enabled) => {
        setAutoStartMonitorState(Boolean(enabled));
        if (enabled) handleManualStartMonitor();
      }).catch(console.warn);
    }
  });

  useEffect(() => {
    checkBackendStatus();
    const interval = setInterval(checkBackendStatus, 3000);
    return () => clearInterval(interval);
  }, [checkBackendStatus]);

  useTauriEventListener('monitor-stopped', (event) => {
    setBackendStatus('offline');
    backendStatusRef.current = 'offline';
    setMonitorPaused(false);
    setBackendError('');
    resetBackendErrorDedupe();
    backendStartAtRef.current = null;
    if (event?.payload?.intentional !== false) return;
    const now = Date.now();
    const recent = unexpectedStopsRef.current.filter((at) => now - at < UNEXPECTED_STOP_WINDOW_MS);
    recent.push(now);
    unexpectedStopsRef.current = recent;
    if (recent.length >= MAX_UNEXPECTED_STOPS) {
      // Leave it stopped until someone starts it again.
      autoStartSuppressedRef.current = true;
      setAutoStartSuppressed(true);
      console.warn(`Capture stopped unexpectedly ${recent.length} times within ${UNEXPECTED_STOP_WINDOW_MS / 60000} minutes; not restarting it automatically`);
    }
  }, [resetBackendErrorDedupe]);

  useEffect(() => {
    if (!autoStartMonitor) return;
    if (autoStartSuppressed || autoStartSuppressedRef.current || runtimeActionRef.current) return;
    if (powerSavingSuppressed) return;
    if (!modelsCheckDone) return;
    if (modelsNeedDownload) return;
    if (backendStatus === 'offline' && backendStatusRef.current !== 'waiting') {
      handleStartBackend();
    }
  }, [
    autoStartMonitor,
    autoStartSuppressed,
    backendStatus,
    handleStartBackend,
    modelsCheckDone,
    modelsNeedDownload,
    powerSavingSuppressed,
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
    handleSettingsMonitorAction,
  };
}
