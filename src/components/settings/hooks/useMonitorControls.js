import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

export function useMonitorControls({
  isOpen,
  onManualStartMonitor,
  onManualStopMonitor,
}) {
  const [monitorStatus, setMonitorStatus] = useState('stopped');
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
      await invoke('start_monitor');
    } catch (e) {
      console.error('Failed to start monitor', e);
      setMonitorStatus('stopped');
      monitorStatusRef.current = 'stopped';
    }
  };

  const handleStopMonitor = async () => {
    setMonitorStatus('loading');
    monitorStatusRef.current = 'loading';
    try {
      await invoke('stop_monitor');
    } catch (e) {
      console.error('Failed to stop monitor', e);
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
      await invoke('stop_monitor');
      setMonitorStatus('waiting');
      monitorStatusRef.current = 'waiting';
      await invoke('start_monitor');
      await checkMonitorStatus();
    } catch (e) {
      console.error('Failed to restart monitor', e);
      setMonitorStatus('stopped');
      monitorStatusRef.current = 'stopped';
      await checkMonitorStatus();
    }
  };

  const handlePauseMonitor = async () => {
    try {
      await invoke('pause_monitor');
      await checkMonitorStatus();
    } catch (e) {
      console.error(e);
    }
  };

  const handleResumeMonitor = async () => {
    try {
      await invoke('resume_monitor');
      await checkMonitorStatus();
    } catch (e) {
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
    handleStartMonitor,
    handleStopMonitor,
    handleRestartMonitor,
    handlePauseMonitor,
    handleResumeMonitor,
  };
}
