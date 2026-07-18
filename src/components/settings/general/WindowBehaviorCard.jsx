import React from 'react';
import { useTranslation } from 'react-i18next';
import { Minimize2 } from 'lucide-react';
import { SettingsButton, SettingsSegmentedControl } from '../SettingsControls';

export default function WindowBehaviorCard({
  lightweightConfig,
  onLightweightConfigChange,
  onSwitchToLightweight,
}) {
  const { t } = useTranslation();

  return (
    <div className="space-y-4">
      <div>
        <label className="block font-semibold text-ide-text mb-1">{t('settings.general.windowBehavior.label')}</label>
        <p className="text-xs text-ide-muted">
          {t('settings.general.windowBehavior.description')}
        </p>
      </div>

      <div className="grid gap-4 md:grid-cols-2">
        <div className="space-y-2">
          <label className="block text-xs font-medium text-ide-text">{t('settings.general.windowBehavior.startup.label')}</label>
          <SettingsSegmentedControl
            value={lightweightConfig.start_with_window_hidden}
            options={[
              {
                value: false,
                label: t('settings.general.windowBehavior.startup.showWindow.label'),
                description: t('settings.general.windowBehavior.startup.showWindow.description'),
              },
              {
                value: true,
                label: t('settings.general.windowBehavior.startup.background.label'),
                description: t('settings.general.windowBehavior.startup.background.description'),
              },
            ]}
            onChange={(next) => onLightweightConfigChange('start_with_window_hidden', next)}
            columns={2}
          />
          <p className="text-xs text-ide-muted">
            {lightweightConfig.start_with_window_hidden
              ? t('settings.general.windowBehavior.startup.background.description')
              : t('settings.general.windowBehavior.startup.showWindow.description')}
          </p>
        </div>

        <div className="space-y-2">
          <label className="block text-xs font-medium text-ide-text">{t('settings.general.windowBehavior.closeWindow.label')}</label>
          <SettingsSegmentedControl
            value={lightweightConfig.auto_lightweight_enabled}
            options={[
              {
                value: false,
                label: t('settings.general.windowBehavior.closeWindow.current.label'),
                description: t('settings.general.windowBehavior.closeWindow.current.description'),
              },
              {
                value: true,
                label: t('settings.general.windowBehavior.closeWindow.background.label'),
                description: t('settings.general.windowBehavior.closeWindow.background.description'),
              },
            ]}
            onChange={(next) => onLightweightConfigChange('auto_lightweight_enabled', next)}
            columns={2}
          />
          <p className="text-xs text-ide-muted">
            {lightweightConfig.auto_lightweight_enabled
              ? t('settings.general.windowBehavior.closeWindow.background.description')
              : t('settings.general.windowBehavior.closeWindow.current.description')}
          </p>
          {lightweightConfig.auto_lightweight_enabled && (
            <div className="flex items-center gap-2">
              <span className="text-xs text-ide-muted">{t('settings.general.windowBehavior.delay.label')}</span>
              <input
                type="number"
                min="1"
                max="60"
                value={lightweightConfig.auto_lightweight_delay_minutes}
                onChange={(e) => {
                  const val = parseInt(e.target.value, 10);
                  if (!isNaN(val) && val >= 1 && val <= 60) {
                    onLightweightConfigChange('auto_lightweight_delay_minutes', val);
                  }
                }}
                onBlur={(e) => {
                  const val = parseInt(e.target.value, 10);
                  if (isNaN(val) || val < 1 || val > 60) {
                    onLightweightConfigChange('auto_lightweight_delay_minutes', 5);
                  }
                }}
                className="w-16 px-2 py-1 bg-ide-panel border border-ide-border rounded text-ide-text text-xs"
              />
              <span className="text-xs text-ide-muted">{t('settings.general.windowBehavior.delay.unit')}</span>
            </div>
          )}
        </div>
      </div>

      <div className="flex items-center justify-between gap-4 rounded-lg border border-ide-border/70 bg-ide-panel/40 px-3 py-2.5">
        <div className="min-w-0">
          <label className="block text-xs font-medium text-ide-text">{t('settings.general.windowBehavior.switchNow.label')}</label>
          <p className="text-xs text-ide-muted">{t('settings.general.windowBehavior.switchNow.description')}</p>
        </div>
        <SettingsButton
          onClick={onSwitchToLightweight}
          icon={Minimize2}
          className="shrink-0"
        >
          {t('settings.general.windowBehavior.switchNow.button')}
        </SettingsButton>
      </div>
    </div>
  );
}
