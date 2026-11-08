import React from 'react';
import { useTranslation } from 'react-i18next';
import { SettingsSegmentedControl, SettingsSelect, SettingsSwitch } from '../SettingsControls';
import { SettingsDivider, SettingsRow, SettingsStatus } from '../SettingsPrimitives';
import { Banner } from '../../ui/Banner';
import { SECURITY_LEVELS, policyForLevel } from './securityLevels';

const COLORS = {
  strict: { selectedClassName: 'border-ide-info-success bg-ide-info-success text-white', idleClassName: 'text-ide-info-success hover:bg-ide-hover' },
  relaxed: { selectedClassName: 'border-ide-accent bg-ide-accent text-white', idleClassName: 'text-ide-accent hover:bg-ide-hover' },
  unattended: { selectedClassName: 'border-ide-warning-border bg-ide-warning-bg text-ide-warning', idleClassName: 'text-ide-warning hover:bg-ide-hover' },
  custom: { selectedClassName: 'border-ide-muted bg-ide-panel text-ide-text', idleClassName: 'text-ide-muted hover:bg-ide-hover' },
};
const TIMEOUTS = [{ value: 300, key: '5m' }, { value: 900, key: '15m' }, { value: 3600, key: '1h' }, { value: 86400, key: '1d' }, { value: -1, key: 'until_close' }];

export default function SecurityPolicyFields({ policy, onChange, disabled = false }) {
  const { t } = useTranslation();
  const levels = SECURITY_LEVELS.map((level) => ({ value: level, label: t(`settings.security.level.options.${level}.label`),
    description: t(`settings.security.level.options.${level}.description`), ...COLORS[level] }));
  const timeouts = TIMEOUTS.map((option) => ({ ...option, label: t(`settings.security.session.options.${option.key}.label`) }));
  if (!timeouts.some((option) => option.value === policy.session_timeout)) {
    timeouts.push({ value: policy.session_timeout, label: t('settings.security.session.currentTimeout', { seconds: policy.session_timeout }) });
  }
  return (
    <div className="space-y-4">
      <SettingsRow label={t('settings.security.level.title')} description={t('settings.security.level.description')}>
        <SettingsSegmentedControl value={policy.level} options={levels} density="card" columns={4} disabled={disabled}
          onChange={(level) => onChange(policyForLevel(level, policy))} />
      </SettingsRow>
      {policy.level === 'custom' && <>
        <SettingsDivider />
        <SettingsRow label={t('settings.security.session.title')} description={t('settings.security.session.description')}
          control={<SettingsSelect value={policy.session_timeout} options={timeouts} disabled={disabled}
            onChange={(session_timeout) => onChange({ ...policy, session_timeout, level: 'custom' })} />} />
        <SettingsRow label={t('settings.security.access.title')} description={t('settings.security.access.description')}
          control={<SettingsSwitch checked={policy.unattended_enabled} disabled={disabled}
            onChange={(unattended_enabled) => onChange({ ...policy, unattended_enabled, level: 'custom' })} />} />
      </>}
      {policy.unattended_enabled && <Banner tone="warning">{t('settings.security.access.active')}</Banner>}
      {!policy.available && <SettingsStatus>{t('settings.security.access.requirements')}</SettingsStatus>}
    </div>
  );
}
