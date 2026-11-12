import React from 'react';
import { useTranslation } from 'react-i18next';
import { ShieldCheck } from 'lucide-react';
import { useAppBoundOffer } from '../hooks/useAppBoundOffer';
import { OverlayShell, useOverlaySlot } from './overlay';
import { Button } from './ui/Button';
import { Banner } from './ui/Banner';

export default function AppBoundUpgradePrompt({ visible }) {
  const { t } = useTranslation();
  const { wanted, busy, error, mode, later, activate } = useAppBoundOffer(visible, t);
  const shown = useOverlaySlot('appBound', wanted);
  if (!shown) return null;
  const upgrade = mode === 'upgrade';
  return (
    <OverlayShell
      icon={ShieldCheck}
      title={t('appBound.title')}
      subtitle={upgrade ? t('appBound.upgradeSubtitle') : t('appBound.subtitle')}
      footer={(
        <>
          <Button size="md" disabled={busy} onClick={later}>
            {t('appBound.later')}
          </Button>
          <Button size="md" variant="primary" icon={ShieldCheck} loading={busy} onClick={activate}>
            {busy ? t('appBound.working') : upgrade ? t('appBound.upgrade')
              : mode === 'repair' ? t('appBound.repair') : t('appBound.enable')}
          </Button>
        </>
      )}
    >
      <p className="text-sm leading-relaxed text-ide-text/90">
        {upgrade ? t('appBound.upgradeDescription') : t('appBound.description')}
      </p>
      <Banner tone="info">
        {upgrade ? t('appBound.upgradeHint') : t('appBound.installHint')}
      </Banner>
      {error && <Banner tone="error">{error}</Banner>}
    </OverlayShell>
  );
}
