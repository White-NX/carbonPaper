import { useEffect, useRef, useState } from 'react';
import { useTauriEventListener } from './useTauriEventListener';

export function useDepsSyncOverlay({
  depsNeedUpdate,
  pythonVersion,
  renderVenvInstallStep,
  depsSyncing,
  onDepsSync,
}) {
  const [depsSyncLog, setDepsSyncLog] = useState([]);
  const [depsSyncError, setDepsSyncError] = useState(null);
  const [depsSyncStarted, setDepsSyncStarted] = useState(false);
  const [retryNonce, setRetryNonce] = useState(0);
  const depsSyncLogRef = useRef(null);
  const prevDepsNeedUpdate = useRef(false);

  useEffect(() => {
    if (depsSyncLogRef?.current) {
      depsSyncLogRef.current.scrollTop = depsSyncLogRef.current.scrollHeight;
    }
  }, [depsSyncLog]);

  useEffect(() => {
    if (!depsNeedUpdate || !pythonVersion || renderVenvInstallStep != null) return;
    if (depsSyncStarted || depsSyncing) return;
    if (depsSyncError) return;

    setDepsSyncStarted(true);
    setDepsSyncLog([]);
    setDepsSyncError(null);

    (async () => {
      try {
        await onDepsSync();
        setDepsSyncStarted(false);
      } catch (err) {
        setDepsSyncError(err?.message || String(err));
        setDepsSyncStarted(false);
      }
    })();
  }, [depsNeedUpdate, pythonVersion, renderVenvInstallStep, depsSyncStarted, depsSyncing, depsSyncError, retryNonce, onDepsSync]);

  useEffect(() => {
    if (depsNeedUpdate && !prevDepsNeedUpdate.current) {
      setDepsSyncLog([]);
      setDepsSyncError(null);
    }
    prevDepsNeedUpdate.current = depsNeedUpdate;
  }, [depsNeedUpdate]);

  useTauriEventListener('install-log', (event) => {
    const payload = event?.payload || {};
    const line = payload.line || JSON.stringify(payload);
    const ts = new Date().toLocaleTimeString();
    setDepsSyncLog((prev) => [...prev, `[${ts}] ${line}`]);
  }, [depsNeedUpdate, depsSyncing], depsNeedUpdate || depsSyncing);

  const retryDepsSync = () => {
    setDepsSyncError(null);
    setDepsSyncStarted(false);
    setRetryNonce((value) => value + 1);
  };

  return {
    depsSyncLog,
    depsSyncError,
    depsSyncLogRef,
    retryDepsSync,
  };
}
