import React from 'react';
import { useTranslation } from 'react-i18next';
import { CheckCircle2, CircleDashed, Loader2, RefreshCw, XCircle } from 'lucide-react';
import { SettingsButton } from '../SettingsControls';

const STATUS_ICON = {
  passed: CheckCircle2,
  failed: XCircle,
  skipped: CircleDashed,
};

const STATUS_CLASS = {
  passed: 'text-green-400',
  failed: 'text-red-400',
  skipped: 'text-ide-muted',
};

export default function McpSmokeTestRow({ loading, disabled = false, report, onRun }) {
  const { t } = useTranslation();

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <label className="block mb-1 font-semibold text-ide-text">
            {t('settings.ai_embedding.smoke_test.title')}
          </label>
          <p className="text-xs text-ide-muted">
            {report
              ? report.ok
                ? t('settings.ai_embedding.smoke_test.success', {
                    count: report.advertised_tool_count,
                    version: report.tool_schema_version,
                  })
                : t(`settings.ai_embedding.smoke_test.errors.${report.failure_kind}`, {
                    defaultValue: report.failure_kind,
                  })
              : t('settings.ai_embedding.smoke_test.description')}
          </p>
        </div>
        <SettingsButton
          icon={loading ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : RefreshCw}
          disabled={loading || disabled}
          onClick={onRun}
        >
          {loading
            ? t('settings.ai_embedding.smoke_test.running')
            : t('settings.ai_embedding.smoke_test.run')}
        </SettingsButton>
      </div>

      {report && (
        <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
          {report.stages.map((stage) => {
            const Icon = STATUS_ICON[stage.status] || CircleDashed;
            return (
              <div key={stage.id} className="min-w-0 text-xs">
                <div className={`flex items-center gap-1.5 ${STATUS_CLASS[stage.status] || STATUS_CLASS.skipped}`}>
                  <Icon className="h-3.5 w-3.5 shrink-0" />
                  <span className="truncate text-ide-text">
                    {t(`settings.ai_embedding.smoke_test.stages.${stage.id}`)}
                  </span>
                </div>
                <span className="ml-5 text-[11px] text-ide-muted">
                  {stage.status === 'skipped'
                    ? t('settings.ai_embedding.smoke_test.skipped')
                    : t('settings.ai_embedding.smoke_test.duration', { duration: stage.duration_ms })}
                </span>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
