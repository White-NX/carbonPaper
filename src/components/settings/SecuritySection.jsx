import React, { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { ShieldCheck } from 'lucide-react';
import { withAuth } from '../../lib/auth_api';
import { notifySettingsChanged } from '../../lib/settings_api';
import { SettingsButton, SettingsSelect } from './SettingsControls';
import { SettingsDivider, SettingsGroup, SettingsRow, SettingsSection, SettingsStatus } from './SettingsPrimitives';
import { useSettingsActivity } from './SettingsActivityContext';

const TIMEOUTS = [{ value: 300, key: '5m' }, { value: 900, key: '15m' }, { value: 3600, key: '1h' }, { value: 86400, key: '1d' }, { value: -1, key: 'until_close' }];
export default function SecuritySection({ sessionTimeout, onSessionTimeoutChange, isSessionValid, onLockSession }) {
  const { t } = useTranslation();
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  useSettingsActivity('session-policy', { busy: saving });
  const options = TIMEOUTS.map((option) => ({ ...option, label: t(`settings.security.session.options.${option.key}.label`) }));
  const current = options.find((option) => option.value === sessionTimeout) || options[1];
  const changeTimeout = async (timeout) => {
    setSaving(true);
    setError('');
    try {
      await withAuth(() => invoke('credential_set_session_timeout', { timeout }), { autoPrompt: true });
      localStorage.setItem('sessionTimeout', String(timeout));
      onSessionTimeoutChange(timeout);
      await notifySettingsChanged(['session']);
    } catch (cause) { setError(t('settings.feedback.saveFailed', { error: String(cause) })); }
    finally { setSaving(false); }
  };
  const lock = async () => {
    setSaving(true);
    try { await invoke('credential_lock_session'); onLockSession?.(); }
    catch (cause) { setError(String(cause)); }
    finally { setSaving(false); }
  };
  return (
    <SettingsSection title={t('settings.security.unlock_label')} icon={ShieldCheck}>
      <SettingsGroup>
        <p className="mb-4 text-xs leading-relaxed text-ide-muted">{t('settings.security.protection.description')}</p>
        <SettingsRow label={t('settings.security.session.title')} description={t('settings.security.session.description')}
          control={<SettingsSelect value={sessionTimeout} options={options} onChange={changeTimeout} disabled={saving} />}>
          <SettingsStatus tone={current.value === -1 ? 'warning' : 'neutral'}>{t(`settings.security.session.options.${current.key}.description`)}</SettingsStatus>
        </SettingsRow>
        <SettingsDivider />
        <div className="flex items-center justify-between gap-3">
          <SettingsStatus tone={isSessionValid ? 'success' : 'neutral'}>{t(isSessionValid ? 'settings.security.session.current.unlocked' : 'settings.security.session.current.locked')}</SettingsStatus>
          {isSessionValid && <SettingsButton variant="ghost" onClick={lock} disabled={saving}>{t('settings.security.session.lock_now')}</SettingsButton>}
        </div>
        {error && <div className="mt-3"><SettingsStatus tone="error">{error}</SettingsStatus></div>}
      </SettingsGroup>
    </SettingsSection>
  );
}
