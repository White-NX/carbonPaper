import React from 'react';
import { useTranslation } from 'react-i18next';
import { Database, RefreshCw } from 'lucide-react';
import { SettingsCard, SettingsDisclosure, SettingsErrorBanner } from '../SettingsPrimitives';
import { SettingsButton } from '../SettingsControls';
import { DiagnosticValues } from '../advanced/InferenceCards';

export default function IndexHealthCard({ indexHealth, indexHealthLoading, indexHealthError, indexBacklog, deleteQueuePending, onRefresh, formatIndexCount, children }) {
  const { t } = useTranslation();
  const key = 'settings.features.management.indexHealth.';
  return <SettingsCard>
    <div className="flex items-center justify-between gap-4">
      <div className="flex min-w-0 items-center gap-2">
        <div className="rounded-lg border border-ide-border bg-ide-bg p-2"><Database className="h-4 w-4" /></div>
        <div><h2 className="text-sm font-semibold">{t(key + 'label')}</h2><p className="mt-1 text-xs text-ide-muted">{t(key + 'description')}</p></div>
      </div>
      <SettingsButton variant="ghost" icon={<RefreshCw className={'h-4 w-4 ' + (indexHealthLoading ? 'animate-spin' : '')} />}
        onClick={onRefresh} disabled={indexHealthLoading} aria-label={t(key + 'refresh')} />
    </div>
    <div className="mt-4 grid grid-cols-2 gap-4 text-xs">
      <div><p className="text-ide-muted">{t(key + 'screenshots')}</p><p className="mt-1 text-sm font-medium tabular-nums">{formatIndexCount(indexHealth?.screenshots_count)}</p></div>
      <div><p className="text-ide-muted">{t(key + 'indexBacklog')}</p><p className="mt-1 text-sm font-medium tabular-nums">{formatIndexCount(indexBacklog)}</p></div>
    </div>
    {indexHealthError && <div className="mt-3"><SettingsErrorBanner>{indexHealthError}</SettingsErrorBanner></div>}
    <SettingsDisclosure className="mt-3" title={t('settings.details.diagnostics')}>
      <DiagnosticValues rows={[
        [t(key + 'ocrRows'), formatIndexCount(indexHealth?.ocr_rows_count)],
        [t(key + 'vectorRows'), formatIndexCount(indexHealth?.vector_rows_count)],
        [t(key + 'deleteQueue'), formatIndexCount(deleteQueuePending)],
        [t(key + 'smartPending'), formatIndexCount(indexHealth?.smart_cluster_pending_count)],
      ]} />
    </SettingsDisclosure>
    {children}
  </SettingsCard>;
}
