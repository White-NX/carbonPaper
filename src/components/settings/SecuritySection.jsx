import React, { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { BookOpen, ShieldCheck } from 'lucide-react';
import { SettingsButton } from './SettingsControls';
import { SettingsDivider, SettingsGroup, SettingsSection, SettingsStatus } from './SettingsPrimitives';
import { useSettingsActivity } from './SettingsActivityContext';
import { Banner } from '../ui/Banner';
import { ProgressBlock } from '../overlay';
import SecurityPolicyFields from './security/SecurityPolicyFields';
import SecurityWizard from './security/SecurityWizard';
import UnattendedWarning from './security/UnattendedWarning';
import { useSecurityLevel } from './security/useSecurityLevel';
import { useSecurityWizard } from './security/useSecurityWizard';
import { securityErrorMessage } from './security/securityLevels';

export default function SecuritySection({ onSessionTimeoutChange, isSessionValid, onLockSession }) {
  const { t } = useTranslation();
  const [locking, setLocking] = useState(false);
  const [wizardOpen, setWizardOpen] = useState(false);
  const [error, setError] = useState('');
  const security = useSecurityLevel({ onSessionTimeoutChange });
  const wizard = useSecurityWizard({ open: wizardOpen, security, onClose: () => setWizardOpen(false) });
  const busy = security.saving || locking || wizard.busy;
  useSettingsActivity('session-lock', { busy: locking });
  const lock = async () => {
    setLocking(true); setError('');
    try { await invoke('credential_lock_session'); onLockSession?.(); }
    catch (cause) { setError(securityErrorMessage(cause, t)); }
    finally { setLocking(false); }
  };
  return (
    <SettingsSection id="security" title={t('settings.security.unlock_label')} icon={ShieldCheck}
      actions={<SettingsButton icon={BookOpen} onClick={() => setWizardOpen(true)} disabled={!security.policy || busy || Boolean(security.pending)}>{t('securityWizard.open')}</SettingsButton>}>
      <SettingsGroup>
        <p className="mb-4 text-xs leading-relaxed text-ide-muted">{t('settings.security.protection.description')}</p>
        {security.loading && <ProgressBlock label={t('settings.security.feedback.loading')} showCount={false} />}
        {security.policy && <SecurityPolicyFields policy={security.policy} onChange={security.requestChange} disabled={busy || Boolean(security.pending) || wizardOpen} />}
        {security.policy && !security.policy.available && <SettingsButton className="mt-3" onClick={security.openComponents} disabled={busy}>{t('settings.security.access.manageComponents')}</SettingsButton>}
        {!security.loading && !security.policy && <SettingsButton onClick={security.refresh}>{t('common.retry')}</SettingsButton>}
        <SettingsDivider />
        <div className="flex items-center justify-between gap-3">
          <SettingsStatus tone={isSessionValid ? 'success' : 'neutral'}>{t(isSessionValid ? 'settings.security.session.current.unlocked' : 'settings.security.session.current.locked')}</SettingsStatus>
          {isSessionValid && <SettingsButton variant="ghost" onClick={lock} loading={locking} disabled={busy || wizardOpen || Boolean(security.pending)}>{t('settings.security.session.lock_now')}</SettingsButton>}
        </div>
        {(error || security.error) && <Banner tone="error" className="mt-3">{error || security.error}</Banner>}
      </SettingsGroup>
      <UnattendedWarning open={Boolean(security.pending)} loading={security.saving} onConfirm={security.confirm} onCancel={security.cancel} />
      <SecurityWizard open={wizardOpen} wizard={wizard} />
    </SettingsSection>
  );
}
