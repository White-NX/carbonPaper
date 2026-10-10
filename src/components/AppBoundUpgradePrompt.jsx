import React, { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ShieldCheck } from 'lucide-react';
import { getAppBoundStatus, installAppBound, dismissAppBoundOffer, appBoundErrorMessage } from '../lib/app_bound_api';
import { OverlayShell, useDebugOverlay, useOverlaySlot } from './overlay';
import { Button } from './ui/Button';
import { Banner } from './ui/Banner';

export default function AppBoundUpgradePrompt({ visible }) {
  const { t } = useTranslation();
  const [offered, setOffered] = useState(false);
  const [debugPreview, setDebugPreview] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [repair, setRepair] = useState(false);
  useEffect(() => {
    if (!visible || debugPreview) return undefined;
    let alive = true;
    getAppBoundStatus().then((status) => {
      if (alive) { setOffered(Boolean(status?.offer_enable)); setRepair(status?.reason === 'repair_required'); }
    }).catch(() => {});
    return () => { alive = false; };
  }, [visible, debugPreview]);
  useDebugOverlay('appBound', (variant) => {
    setRepair(variant === 'repair');
    setError('');
    setBusy(false);
    setDebugPreview(true);
  });
  const shown = useOverlaySlot('appBound', debugPreview || (visible && offered));
  if (!shown) return null;
  const later = async () => {
    if (debugPreview) {
      setDebugPreview(false);
      return;
    }
    try { await dismissAppBoundOffer(); setOffered(false); }
    catch (failure) { setError(appBoundErrorMessage(failure, t)); }
  };
  const enable = async () => {
    setBusy(true); setError('');
    try { await installAppBound(!repair); setOffered(false); }
    catch (failure) { setError(appBoundErrorMessage(failure, t)); }
    finally { setBusy(false); }
  };
  return (
    <OverlayShell
      icon={ShieldCheck}
      title={t('appBound.title')}
      subtitle={t('appBound.subtitle')}
      footer={(
        <>
          <Button size="md" disabled={busy} onClick={later}>
            {t('appBound.later')}
          </Button>
          <Button size="md" variant="primary" icon={ShieldCheck} loading={busy} onClick={enable}>
            {busy ? t('appBound.working') : repair ? t('appBound.repair') : t('appBound.enable')}
          </Button>
        </>
      )}
    >
      <p className="text-sm leading-relaxed text-ide-text/90">
        {t('appBound.description')}
      </p>
      <Banner tone="info">
        {t('appBound.installHint')}
      </Banner>
      {error && <Banner tone="error">{error}</Banner>}
    </OverlayShell>
  );
}
