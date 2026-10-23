import React, { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2 } from 'lucide-react';
import { SemanticBackendCard, ClipBackendCard } from './advanced/InferenceCards';
import { SettingsDisclosure, SettingsDivider, SettingsRow, SettingsStatus } from './SettingsPrimitives';
import { SettingsButton } from './SettingsControls';
import { ProgressBlock } from '../overlay/ProgressBlock';

export default function IndexMaintenanceSection({ indexes, thumbnails, vacuum }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const needsAttention = [indexes.semantic.phase, indexes.clip.phase].some((phase) => ['running', 'queued', 'stopping', 'waiting_for_unlock', 'waiting_for_verification', 'retry_wait', 'failed'].includes(phase))
    || (indexes.status?.backend?.index_stalled ?? 0) > 0 || (indexes.status?.clip_backend?.index_stalled ?? 0) > 0
    || ['backoff', 'circuit_open'].includes(indexes.status?.clip_backend?.ann_build_state);
  useEffect(() => { if (needsAttention) setOpen(true); }, [needsAttention]);
  return <>
    <SettingsDivider />
    <SettingsDisclosure title={t('settings.maintenance.manageIndexes')} open={open} onOpenChange={setOpen}>
      <SemanticBackendCard status={indexes.status} statusLoading={indexes.statusLoading} onRefresh={indexes.refresh}
        onRunIndexNow={indexes.semantic.run} onStopIndexNow={indexes.semantic.stop}
        indexRunning={indexes.semantic.running} indexPhase={indexes.semantic.phase} indexRetryAt={indexes.semantic.retryAt}
        indexStopping={indexes.semantic.stopping} indexProgress={indexes.semantic.progress} indexRun={indexes.semantic.summary} indexError={indexes.semantic.error} />
      <SettingsDivider />
      <ClipBackendCard status={indexes.status} statusLoading={indexes.statusLoading} onRefresh={indexes.refresh}
        onRunIndexNow={indexes.clip.run} onStopIndexNow={indexes.clip.stop} onRetryAnn={indexes.ann.retry} annRetrying={indexes.ann.retrying}
        indexRunning={indexes.clip.running} indexPhase={indexes.clip.phase} indexRetryAt={indexes.clip.retryAt} indexStopping={indexes.clip.stopping}
        indexProgress={indexes.clip.progress} indexRun={indexes.clip.summary} indexError={indexes.clip.error}
        backfill={indexes.backfill.offer} backfillBusy={indexes.backfill.busy} onBackfillDecision={indexes.backfill.setDecision} />
    </SettingsDisclosure>
    {(thumbnails.status?.running || thumbnails.status?.cancel_requested || thumbnails.error) && <>
      <SettingsDivider />
      <SettingsRow label={t('settings.maintenance.thumbnails.title')} description={t('settings.maintenance.thumbnails.description')}
        control={<SettingsButton onClick={thumbnails.toggle}
          disabled={thumbnails.busy || (thumbnails.status?.running && thumbnails.status.cancel_requested)}>
          {t(thumbnails.status?.running ? 'settings.maintenance.thumbnails.stop' : 'settings.maintenance.thumbnails.resume')}
        </SettingsButton>}>
        {thumbnails.status?.running
          ? <ProgressBlock current={thumbnails.status.processed} total={thumbnails.status.total}
            label={t(thumbnails.status.cancel_requested ? 'settings.maintenance.thumbnails.stopping' : 'settings.maintenance.thumbnails.running')} />
          : thumbnails.status?.cancel_requested && <SettingsStatus>{t('settings.maintenance.thumbnails.stopped')}</SettingsStatus>}
        {thumbnails.error && <SettingsStatus tone="error">{thumbnails.error}</SettingsStatus>}
      </SettingsRow>
    </>}
    <SettingsDivider />
    <SettingsRow label={t('settings.advanced.vacuum.label')} description={t('settings.advanced.vacuum.description')}
      control={<SettingsButton onClick={vacuum.run} disabled={vacuum.running}
        icon={vacuum.running ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : undefined}>
        {t(vacuum.running ? 'settings.advanced.vacuum.running' : 'settings.advanced.vacuum.action')}
      </SettingsButton>} />
    {vacuum.message && <SettingsStatus>{vacuum.message}</SettingsStatus>}
  </>;
}
