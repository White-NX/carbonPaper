import React, { useId } from 'react';
import { useTranslation } from 'react-i18next';

export default function BackgroundSchedulingCard({ value = 'auto', saving, error, onChange }) {
  const { t } = useTranslation();
  const groupId = useId();
  const options = [
    { value: 'auto', label: t('settings.features.backgroundTiming.auto') },
    { value: 'idle_only', label: t('settings.features.backgroundTiming.idleOnly') },
  ];
  return (
    <fieldset className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-3" disabled={saving}>
      <legend className="px-1 text-sm font-medium text-ide-text">
        {t('settings.features.backgroundTiming.title')}
      </legend>
      <div className="grid grid-cols-2 gap-2">
        {options.map((option) => (
          <label key={option.value} className={`flex items-center gap-2 rounded-lg border px-3 py-2 text-sm cursor-pointer ${value === option.value ? 'border-ide-accent text-ide-text bg-ide-panel' : 'border-ide-border text-ide-muted'}`}>
            <input type="radio" name={groupId} value={option.value}
              checked={value === option.value} onChange={() => onChange(option.value)}
              className="accent-ide-accent" />
            {option.label}
          </label>
        ))}
      </div>
      <p className="text-xs text-ide-muted">
        {value === 'idle_only' ? t('settings.features.backgroundTiming.idleDescription') : t('settings.features.backgroundTiming.autoDescription')}
      </p>
      {error && <p role="alert" className="text-xs text-red-400">{t('settings.features.backgroundTiming.saveError')}</p>}
    </fieldset>
  );
}
