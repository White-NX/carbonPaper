import { useCallback, useEffect, useState } from 'react';
import {
  getAppBoundStatus, installAppBound, dismissAppBoundOffer, appBoundErrorMessage,
} from '../lib/app_bound_api';
import { useDebugOverlay } from '../components/overlay';
import { useAppBoundRepair } from './useAppBoundRepair';

export function useAppBoundOffer(visible, t) {
  const [offered, setOffered] = useState(false);
  const [debugPreview, setDebugPreview] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [mode, setMode] = useState('enable');
  const repaired = useCallback(() => setOffered(false), []);
  const repair = useAppBoundRepair(t, repaired);

  useEffect(() => {
    if (!visible || debugPreview) return undefined;
    let alive = true;
    getAppBoundStatus().then((status) => {
      if (!alive) return;
      setOffered(Boolean(status?.offer_enable));
      setMode(status?.reason === 'upgrade_required' ? 'upgrade'
        : status?.reason === 'repair_required' ? 'repair' : 'enable');
    }).catch(() => {});
    return () => { alive = false; };
  }, [visible, debugPreview]);

  useDebugOverlay('appBound', (variant) => {
    setMode(['repair', 'upgrade'].includes(variant) ? variant : 'enable');
    setError('');
    setBusy(false);
    setDebugPreview(true);
  });

  const later = async () => {
    if (debugPreview) {
      setDebugPreview(false);
      return;
    }
    try { await dismissAppBoundOffer(); setOffered(false); }
    catch (failure) { setError(appBoundErrorMessage(failure, t)); }
  };
  const activate = async () => {
    if (debugPreview) {
      setDebugPreview(false);
      return;
    }
    if (mode === 'repair') {
      await repair.repair();
      return;
    }
    setBusy(true);
    setError('');
    try { await installAppBound(mode === 'enable'); setOffered(false); }
    catch (failure) { setError(appBoundErrorMessage(failure, t)); }
    finally { setBusy(false); }
  };
  return { wanted: debugPreview || (visible && offered), busy: busy || repair.busy,
    error: repair.error || error, mode, later, activate, repair };
}
