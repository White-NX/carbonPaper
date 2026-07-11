import React from 'react';
import { useTranslation } from 'react-i18next';
import { Globe } from 'lucide-react';
import { SettingsSwitch } from '../SettingsControls';

export default function NetworkAccessCard({
  config,
  onToggle,
}) {
  const { t } = useTranslation();

  return (
    <div className="space-y-3">
      <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
        <Globe className="w-4 h-4" />
        {t('settings.advanced.network.title')}
      </label>

      <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
        <div className="flex items-center justify-between gap-4">
          <div className="flex-1 min-w-0">
            <p className="text-sm text-ide-text font-medium">{t('settings.advanced.network.label')}</p>
            <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.network.description')}</p>
          </div>
          <SettingsSwitch
            checked={config.network_enabled}
            onChange={() => onToggle('network_enabled')}
          />
        </div>
      </div>
    </div>
  );
}
