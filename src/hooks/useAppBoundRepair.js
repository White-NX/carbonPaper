import { useCallback, useEffect, useRef, useState } from 'react';
import { repairAppBound, appBoundErrorMessage } from '../lib/app_bound_api';
import { selectAppBoundRepairPackage } from '../lib/native_dialogs';
import { useTauriEventListener } from './useTauriEventListener';

export function useAppBoundRepair(t, onRepaired) {
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState(null);
  const [error, setError] = useState('');
  const [downloadRequest, setDownloadRequest] = useState(null);
  const running = useRef(false);
  const mounted = useRef(false);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  useTauriEventListener('app-bound-repair-progress', ({ payload }) => {
    if (running.current) setProgress(payload);
  }, [], busy);

  const repair = useCallback(async (options = {}) => {
    if (running.current) return false;
    running.current = true;
    setBusy(true);
    setError('');
    setDownloadRequest(null);
    setProgress({ phase: 'checking', downloaded: 0, total: 0 });
    try {
      const result = await repairAppBound(options);
      if (!mounted.current) return false;
      if (result?.status === 'download_required') {
        setDownloadRequest({ sizeBytes: result.size_bytes, options });
        return false;
      }
      await onRepaired?.();
      return true;
    } catch (failure) {
      if (mounted.current) {
        if (String(failure).includes('REPAIR_FULL_RUNTIME_REQUIRED') && !options.packagePath) {
          setDownloadRequest({ sizeBytes: null, options });
        } else { setError(appBoundErrorMessage(failure, t)); }
      }
      return false;
    } finally {
      running.current = false;
      if (mounted.current) setBusy(false);
    }
  }, [t, onRepaired]);

  const repairFromFile = useCallback(async () => {
    if (running.current) return false;
    try {
      const packagePath = await selectAppBoundRepairPackage();
      if (packagePath && mounted.current) return repair({ packagePath });
    } catch (failure) { if (mounted.current) setError(appBoundErrorMessage(failure, t)); }
    return false;
  }, [repair, t]);
  const confirmDownload = () => repair({ ...downloadRequest?.options, allowFullDownload: true });
  const cancelDownload = () => setDownloadRequest(null);
  return { busy, progress, error, downloadRequest, repair, repairFromFile, confirmDownload, cancelDownload };
}
