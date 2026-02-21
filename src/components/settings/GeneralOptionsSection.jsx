import React from 'react';
import { useTranslation } from 'react-i18next';

export default function GeneralOptionsSection({
  lowResolutionAnalysis,
  onToggleLowRes,
  sendTelemetryDiagnostics,
  onToggleTelemetry,
}) {
  const { t } = useTranslation();

  return (
    <div className="space-y-3">
      <label className="text-sm font-semibold text-ide-accent px-1 block">{t('settings.general.title')}</label>
      <div className="p-4 bg-ide-bg border border-ide-border rounded-xl text-sm text-ide-muted space-y-3">
        <div className="flex items-center justify-between gap-4">
          <div>
            <label className="block font-semibold text-ide-text mb-1">{t('settings.general.telemetry.label')}</label>
            <p className="text-xs text-ide-muted">
              {t('settings.general.telemetry.description')}
            </p>
          </div>
          <button
            onClick={onToggleTelemetry}
            className={`w-11 h-6 shrink-0 rounded-full transition-colors relative ${sendTelemetryDiagnostics ? 'bg-ide-accent' : 'bg-ide-panel border border-ide-border'
              }`}
            title={t('settings.general.telemetry.label')}
          >
            <div
              className="absolute top-1 w-4 h-4 rounded-full bg-white transition-transform shadow-sm"
              style={{ left: sendTelemetryDiagnostics ? 'calc(100% - 1.25rem)' : '0.25rem' }}
            />
          </button>
        </div>

        {/*<div className="w-full h-px bg-ide-border/50" />*/}

      </div>
    </div>
  );
}
