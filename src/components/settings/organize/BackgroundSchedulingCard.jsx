import React from 'react';
import { SettingsSegmentedControl } from '../SettingsControls';
import { SettingsGroup, SettingsRow } from '../SettingsPrimitives';
import { useTranslation } from 'react-i18next';

export default function BackgroundSchedulingCard({ value = 'auto', saving, error, onChange }) {
  const { t } = useTranslation();
  const options = [
    { value: 'auto', label: t('settings.features.backgroundTiming.auto') },
    { value: 'idle_only', label: t('settings.features.backgroundTiming.idleOnly') },
  ];
  return (
    <SettingsGroup>
      <SettingsRow label={t('settings.features.backgroundTiming.title')}>
        <SettingsSegmentedControl value={value} options={options} onChange={onChange} disabled={saving} columns={2} />
        <p className="text-xs text-ide-muted">{t(value === 'idle_only' ? 'settings.features.backgroundTiming.idleDescription' : 'settings.features.backgroundTiming.autoDescription')}</p>
        {error && <p role="alert" className="text-xs text-ide-error">{t('settings.features.backgroundTiming.saveError')}</p>}
      </SettingsRow>
    </SettingsGroup>
  );
}
