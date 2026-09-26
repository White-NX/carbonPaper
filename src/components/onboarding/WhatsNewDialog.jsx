import React from 'react';
import { useTranslation } from 'react-i18next';
import { ShieldCheck, Sparkles } from 'lucide-react';
import { OverlayShell } from '../overlay';
import { Button } from '../ui/Button';
import { Banner } from '../ui/Banner';
import { SwitchRow } from './OptionCards';

const ENTRY_ICONS = { smartCluster: Sparkles, appBound: ShieldCheck };

/**
 * The short page existing users see once instead of the full wizard: only
 * the features that would change something for them, each on by default.
 */
export default function WhatsNewDialog({ onboarding }) {
  const { t } = useTranslation();
  const { whatsNewEntries, whatsNewSelection, whatsNewError, phase } = onboarding;
  const busy = phase === 'applying';
  const anySelected = whatsNewEntries.some((entry) => whatsNewSelection[entry.id]);

  return (
    <OverlayShell
      size="lg"
      onDismiss={busy ? undefined : onboarding.remindLater}
      icon={Sparkles}
      title={t('onboarding.whatsNew.title')}
      subtitle={t('onboarding.whatsNew.subtitle')}
      footerStart={(
        <Button size="md" variant="ghost" disabled={busy} onClick={onboarding.remindLater}>
          {t('onboarding.whatsNew.later')}
        </Button>
      )}
      footer={(
        <>
          <Button size="md" disabled={busy} onClick={onboarding.skipWhatsNew}>{t('onboarding.whatsNew.skip')}</Button>
          <Button size="md" variant="primary" loading={busy} disabled={!anySelected} onClick={onboarding.applyWhatsNew}>
            {t('onboarding.whatsNew.apply')}
          </Button>
        </>
      )}
    >
      <div className="divide-y divide-ide-border/60 rounded-xl border border-ide-border bg-ide-bg px-3">
        {whatsNewEntries.map((entry) => (
          <SwitchRow
            key={entry.id}
            icon={ENTRY_ICONS[entry.id]}
            checked={Boolean(whatsNewSelection[entry.id])}
            onChange={(on) => onboarding.setWhatsNewChoice(entry.id, on)}
            label={t(`onboarding.whatsNew.entries.${entry.id}.title`)}
            description={t(`onboarding.whatsNew.entries.${entry.id}.description`, { size: entry.sizeMb })}
            note={entry.id === 'appBound' ? t('onboarding.appBound.note') : undefined}
          />
        ))}
      </div>
      {whatsNewError && <Banner tone="error">{whatsNewError}</Banner>}
    </OverlayShell>
  );
}
