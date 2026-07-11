import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../../lib/auth_api';

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
      await withAuth(() => invoke('start_monitor'), { autoPrompt: true });
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
      await withAuth(() => invoke('stop_monitor'), { autoPrompt: true });
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
      await withAuth(() => invoke('stop_monitor'), { autoPrompt: true });
      setMonitorStatus('waiting');
      monitorStatusRef.current = 'waiting';
      await withAuth(() => invoke('start_monitor'), { autoPrompt: true });
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
      await withAuth(() => invoke('pause_monitor'), { autoPrompt: true });
      await checkMonitorStatus();
    } catch (e) {
      console.error(e);
    }
  };

  const handleResumeMonitor = async () => {
    try {
      await withAuth(() => invoke('resume_monitor'), { autoPrompt: true });
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
