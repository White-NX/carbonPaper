import React, { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2 } from 'lucide-react';
import { SemanticBackendCard, ClipBackendCard } from './advanced/InferenceCards';
import { SettingsDisclosure, SettingsDivider, SettingsRow, SettingsStatus } from './SettingsPrimitives';
import { SettingsButton } from './SettingsControls';
import { ProgressBlock } from '../overlay/ProgressBlock';

export default function IndexMaintenanceSection({ controller: r }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const needsAttention = [r.semanticIndexPhase, r.clipIndexPhase].some((phase) => ['running', 'queued', 'stopping', 'waiting_for_unlock', 'waiting_for_verification', 'retry_wait', 'failed'].includes(phase))
    || (r.semanticStatus?.backend?.index_stalled ?? 0) > 0 || (r.semanticStatus?.clip_backend?.index_stalled ?? 0) > 0
    || ['backoff', 'circuit_open'].includes(r.semanticStatus?.clip_backend?.ann_build_state);
  useEffect(() => { if (needsAttention) setOpen(true); }, [needsAttention]);
  return <>
    <SettingsDivider />
    <SettingsDisclosure title={t('settings.maintenance.manageIndexes')} open={open} onOpenChange={setOpen}>
      <SemanticBackendCard status={r.semanticStatus} statusLoading={r.semanticStatusLoading} onRefresh={r.refreshSemanticStatus}
        onRunIndexNow={r.handleRunSemanticIndexNow} onStopIndexNow={r.handleStopSemanticIndex}
        indexRunning={r.semanticIndexRunning} indexPhase={r.semanticIndexPhase} indexRetryAt={r.semanticIndexRetryAt}
        indexStopping={r.semanticIndexStopping} indexProgress={r.semanticIndexProgress} indexRun={r.semanticIndexRun} indexError={r.semanticIndexError} />
      <SettingsDivider />
      <ClipBackendCard status={r.semanticStatus} statusLoading={r.semanticStatusLoading} onRefresh={r.refreshSemanticStatus}
        onRunIndexNow={r.handleRunClipIndexNow} onStopIndexNow={r.handleStopClipIndex} onRetryAnn={r.handleRetryClipAnn} annRetrying={r.clipAnnRetrying}
        indexRunning={r.clipIndexRunning} indexPhase={r.clipIndexPhase} indexRetryAt={r.clipIndexRetryAt} indexStopping={r.clipIndexStopping}
        indexProgress={r.clipIndexProgress} indexRun={r.clipIndexRun} indexError={r.clipIndexError} backfill={r.clipBackfill} backfillBusy={r.clipBackfillBusy} onBackfillDecision={r.handleClipBackfillDecision} />
    </SettingsDisclosure>
    {(r.thumbnailWarmup?.running || r.thumbnailWarmup?.cancel_requested || r.thumbnailWarmupError) && <>
      <SettingsDivider />
      <SettingsRow label={t('settings.maintenance.thumbnails.title')} description={t('settings.maintenance.thumbnails.description')}
        control={<SettingsButton onClick={r.handleThumbnailWarmup}
          disabled={r.thumbnailWarmupBusy || (r.thumbnailWarmup?.running && r.thumbnailWarmup.cancel_requested)}>
          {t(r.thumbnailWarmup?.running ? 'settings.maintenance.thumbnails.stop' : 'settings.maintenance.thumbnails.resume')}
        </SettingsButton>}>
        {r.thumbnailWarmup?.running
          ? <ProgressBlock current={r.thumbnailWarmup.processed} total={r.thumbnailWarmup.total}
            label={t(r.thumbnailWarmup.cancel_requested ? 'settings.maintenance.thumbnails.stopping' : 'settings.maintenance.thumbnails.running')} />
          : r.thumbnailWarmup?.cancel_requested && <SettingsStatus>{t('settings.maintenance.thumbnails.stopped')}</SettingsStatus>}
        {r.thumbnailWarmupError && <SettingsStatus tone="error">{r.thumbnailWarmupError}</SettingsStatus>}
      </SettingsRow>
    </>}
    <SettingsDivider />
    <SettingsRow label={t('settings.advanced.vacuum.label')} description={t('settings.advanced.vacuum.description')}
      control={<SettingsButton onClick={r.handleManualVacuum} disabled={r.vacuumRunning}
        icon={r.vacuumRunning ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : undefined}>
        {t(r.vacuumRunning ? 'settings.advanced.vacuum.running' : 'settings.advanced.vacuum.action')}
      </SettingsButton>} />
    {r.vacuumMessage && <SettingsStatus>{r.vacuumMessage}</SettingsStatus>}
  </>;
}
