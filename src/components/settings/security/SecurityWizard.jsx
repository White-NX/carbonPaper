import React from 'react';
import { useTranslation } from 'react-i18next';
import { Bot, ShieldAlert } from 'lucide-react';
import { Dialog } from '../../Dialog';
import { OverlayShell } from '../../overlay';
import { Button } from '../../ui/Button';
import { Banner } from '../../ui/Banner';
import SecurityPolicyFields from './SecurityPolicyFields';

/** Plain view shared by the startup introduction and settings dialog. */
export default function SecurityWizard({ open, wizard, presentation = 'dialog' }) {
  const { t } = useTranslation();
  if (!open || !wizard.draft) return null;
  const warning = Boolean(wizard.warning);
  const title = warning ? t('settings.security.warning.title') : t('securityWizard.title');
  const body = warning ? <Banner tone="warning" title={t('settings.security.warning.title')}>
    <p className="whitespace-pre-line">{t('settings.security.warning.message')}</p>
  </Banner> : <>
    <p className="text-xs text-ide-muted">{t('securityWizard.step', { current: wizard.step + 1, total: 3 })}</p>
    <h3 className="text-base font-semibold text-ide-text">{t(`securityWizard.steps.${wizard.step}.title`)}</h3>
    {wizard.step === 0 && <div className="space-y-3 text-sm leading-relaxed text-ide-muted">
      <p>{t('securityWizard.intro.history')}</p><p>{t('securityWizard.intro.startup')}</p>
      <Banner tone="info">{t('securityWizard.intro.scope')}</Banner>
    </div>}
    {wizard.step === 1 && <div className="space-y-3 text-sm leading-relaxed text-ide-muted">
      <Banner tone="warning" title={t('securityWizard.risk.title')}>{t('securityWizard.risk.lock')}</Banner>
      <p>{t('securityWizard.risk.credential')}</p><p>{t('securityWizard.risk.recommendation')}</p>
      <p>{t('securityWizard.risk.revoke')}</p>
    </div>}
    {wizard.step === 2 && <div className="settings-surface">
      <SecurityPolicyFields policy={wizard.draft} onChange={wizard.change} disabled={wizard.busy} />
      <p className="mt-4 text-xs leading-relaxed text-ide-muted">{t('securityWizard.credentials')}</p>
    </div>}
  </>;
  const footer = warning ? <>
    <Button size="md" disabled={wizard.busy} onClick={wizard.cancelRisk}>{t('common.cancel')}</Button>
    <Button size="md" variant="danger" disabled={wizard.busy} onClick={wizard.acceptRisk}>{t('settings.security.warning.confirm')}</Button>
  </> : <>
    {wizard.step > 0 && <Button size="md" variant="ghost" disabled={wizard.busy} onClick={wizard.back}>{t('securityWizard.back')}</Button>}
    {wizard.step < 2 ? <Button size="md" variant="primary" disabled={wizard.busy} onClick={wizard.next}>{t('securityWizard.next')}</Button>
      : <Button size="md" variant="primary" loading={wizard.busy} disabled={wizard.draft.unattended_enabled && !wizard.draft.available} onClick={wizard.finish}>{t('securityWizard.apply')}</Button>}
  </>;
  const later = <Button size="md" variant="ghost" disabled={wizard.busy || warning} onClick={wizard.close}>{t('securityWizard.later')}</Button>;
  const contents = <>{body}{wizard.error && <Banner tone="error">{wizard.error}</Banner>}</>;
  const dismiss = warning ? wizard.cancelRisk : wizard.close;
  if (presentation === 'startup') return <OverlayShell open size="xl" title={title} icon={warning ? ShieldAlert : Bot}
    tone={warning ? 'warning' : 'accent'} onDismiss={wizard.busy ? undefined : dismiss} footerStart={later} footer={footer}>{contents}</OverlayShell>;
  return <Dialog isOpen onClose={dismiss} title={title} maxWidth="max-w-2xl" disableClose={wizard.busy}>
    <div className="space-y-4 p-5">{contents}</div>
    <div className="flex flex-wrap items-center justify-between gap-3 border-t border-ide-border bg-ide-panel p-4">
      {later}<div className="flex flex-wrap items-center gap-2">{footer}</div>
    </div>
  </Dialog>;
}
