import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { runMonitorAction } from '../../../lib/settings_api';

export function useMonitorControls({
  isOpen,
  onManualStartMonitor,
  onManualStopMonitor,
}) {
  const [monitorStatus, setMonitorStatus] = useState('stopped');
  const [monitorError, setMonitorError] = useState('');
  const monitorStatusRef = useRef('stopped');

  const checkMonitorStatus = useCallback(async () => {
    try {
      const resString = await invoke('get_monitor_status');
      try {
        const res = JSON.parse(resString);
        if (res.stopped) {
          setMonitorStatus('stopped');
          monitorStatusRef.current = 'stopped';
        } else if (res.paused) {
          setMonitorStatus('paused');
          monitorStatusRef.current = 'paused';
        } else {
          setMonitorStatus('running');
          monitorStatusRef.current = 'running';
        }
      } catch {
        setMonitorStatus('running');
        monitorStatusRef.current = 'running';
      }
    } catch {
      if (monitorStatusRef.current === 'waiting') {
        return;
      }
      setMonitorStatus('stopped');
      monitorStatusRef.current = 'stopped';
    }
  }, []);

  const handleStartMonitor = async () => {
    setMonitorStatus('waiting');
    monitorStatusRef.current = 'waiting';
    onManualStartMonitor?.();
    try {
      setMonitorError('');
      await runMonitorAction('start');
      await checkMonitorStatus();
      return true;
    } catch (e) {
      console.error('Failed to start monitor', e);
      setMonitorError(String(e));
      setMonitorStatus('stopped');
      monitorStatusRef.current = 'stopped';
      return false;
    }
  };

  const handleStopMonitor = async () => {
    setMonitorStatus('loading');
    monitorStatusRef.current = 'loading';
    try {
      setMonitorError('');
      await runMonitorAction('stop');
    } catch (e) {
      console.error('Failed to stop monitor', e);
      setMonitorError(String(e));
    } finally {
      onManualStopMonitor?.();
      setMonitorStatus('stopped');
      monitorStatusRef.current = 'stopped';
    }
  };

  const handleRestartMonitor = async () => {
    setMonitorStatus('loading');
    monitorStatusRef.current = 'loading';
    try {
      setMonitorError('');
      await runMonitorAction('restart');
      await checkMonitorStatus();
      return true;
    } catch (e) {
      console.error('Failed to restart monitor', e);
      setMonitorError(String(e));
      setMonitorStatus('stopped');
      monitorStatusRef.current = 'stopped';
      await checkMonitorStatus();
      return false;
    }
  };

  const handlePauseMonitor = async () => {
    try {
      setMonitorError('');
      await runMonitorAction('pause');
      await checkMonitorStatus();
    } catch (e) {
      setMonitorError(String(e));
      console.error(e);
    }
  };

  const handleResumeMonitor = async () => {
    try {
      setMonitorError('');
      await runMonitorAction('resume');
      await checkMonitorStatus();
    } catch (e) {
      setMonitorError(String(e));
      console.error(e);
    }
  };

  useEffect(() => {
    let interval;
    if (isOpen) {
      checkMonitorStatus();
      interval = setInterval(checkMonitorStatus, 2000);
    }
    return () => clearInterval(interval);
  }, [isOpen, checkMonitorStatus]);

  useEffect(() => {
    monitorStatusRef.current = monitorStatus;
  }, [monitorStatus]);

  return {
    monitorStatus,
    monitorError,
    handleStartMonitor,
    handleStopMonitor,
    handleRestartMonitor,
    handlePauseMonitor,
    handleResumeMonitor,
  };
}
