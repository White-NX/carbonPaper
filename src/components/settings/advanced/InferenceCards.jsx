import React from 'react';
import { useTranslation } from 'react-i18next';
import { RefreshCw } from 'lucide-react';
import { SettingsButton, SettingsSwitch } from '../SettingsControls';
import { SettingsDisclosure, SettingsDivider, SettingsGroup, SettingsRow, SettingsSection, SettingsStatus, SettingsWarningBanner } from '../SettingsPrimitives';
import { formatEstimate } from '../../ClipBackfillDialog';

export function DiagnosticValues({ rows }) {
  return <dl className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)] gap-x-4 gap-y-2 text-xs">
    {rows.map(([label, value]) => <React.Fragment key={label}><dt className="text-ide-muted">{label}</dt><dd className="break-all text-right text-ide-text">{value ?? '—'}</dd></React.Fragment>)}
  </dl>;
}

export function BackgroundSchedulerCard({ enabled, saving, status, onChange, onRefresh }) {
  const { t } = useTranslation();
  const state = status?.running_task ? 'running' : status?.blocked_reason || (status?.next_retry_at_ms ? 'retry_wait' : 'waiting_for_idle');
  const mapped = { clustering_already_running: 'organizing', waiting_for_index: 'preparing', waiting_for_ac_power: 'waiting_for_idle', waiting_for_fullscreen: 'waiting_for_idle', foreground_request: 'waiting_for_idle', maintenance: 'waiting_for_idle', semantic_worker_busy: 'waiting_for_idle' }[state] || state;
  const known = ['running', 'waiting_for_idle', 'waiting_for_unlock', 'waiting_for_verification', 'retry_wait', 'failed', 'monitor_unavailable', 'disabled', 'organizing', 'preparing'];
  const label = t(`settings.advanced.background_processing.states.${(known.includes(mapped) ? mapped : 'waiting_for_idle')}`);
  return (
    <SettingsSection id="background-access" title={t('settings.advanced.background_processing.title')}>
      <SettingsGroup>
        <SettingsRow label={t('settings.advanced.background_processing.label')} description={t('settings.advanced.background_processing.description')}
          control={<SettingsSwitch checked={enabled} disabled={saving || !status} onChange={onChange} />} />
        {enabled && <><SettingsDivider /><div className="flex items-center justify-between gap-3">
          <SettingsStatus>{status ? label : t('settings.feedback.loading')}</SettingsStatus>
          <SettingsButton variant="ghost" icon={RefreshCw} onClick={onRefresh} aria-label={t('settings.advanced.background_processing.refresh')} />
        </div></>}
      </SettingsGroup>
    </SettingsSection>
  );
}

export function OcrEngineCard({ status, statusLoading, modelStatus, modelDownloading, onRestart, onDownloadModel, onRefresh, error }) {
  const { t } = useTranslation();
  const state = error && !modelStatus ? 'unavailable' : !modelStatus ? 'checking' : modelStatus.installed ? 'ready' : 'repair';
  return (
    <SettingsGroup id="ocr-repair">
      <SettingsRow label={t('settings.advanced.ocrHealth.title')} description={t(`settings.advanced.ocrHealth.${state}`)}
        control={state === 'repair' ? <SettingsButton variant="primary" disabled={modelDownloading} onClick={onDownloadModel}>
          {t(modelDownloading ? 'settings.advanced.rust_ocr.model_downloading' : 'settings.advanced.rust_ocr.model_repair')}
        </SettingsButton> : <SettingsButton variant="ghost" icon={RefreshCw} onClick={onRefresh} disabled={statusLoading || modelDownloading} aria-label={t('settings.advanced.ocrHealth.refresh')} />} />
      <SettingsDisclosure title={t('settings.details.diagnostics')}>
        <DiagnosticValues rows={[
          [t('settings.advanced.ocrHealth.state'), status?.state],
          [t('settings.advanced.ocrHealth.provider'), status?.provider],
          [t('settings.advanced.ocrHealth.model'), status?.model_id],
          [t('settings.advanced.ocrHealth.success'), status?.success_count],
          [t('settings.advanced.ocrHealth.failure'), status?.failure_count],
          [t('settings.advanced.ocrHealth.elapsed'), status?.last_elapsed_ms == null ? '—' : Math.round(status.last_elapsed_ms) + ' ms'],
        ]} />
        {modelStatus?.path && <p className="break-all font-mono text-xs text-ide-muted">{modelStatus.path}</p>}
        <SettingsButton disabled={modelDownloading || !modelStatus} onClick={onRestart} icon={RefreshCw}>{t('settings.advanced.ocrHealth.restart')}</SettingsButton>
      </SettingsDisclosure>
    </SettingsGroup>
  );
}

function retryTime(value) {
  if (value == null || value === '') return null;
  const numeric = typeof value === 'number' ? value : Number(value);
  const date = new Date(Number.isFinite(numeric) ? numeric : value);
  return Number.isNaN(date.getTime()) ? null : date.toLocaleString();
}

export function SemanticBackendCard(props) { return <IndexTask kind="semantic" {...props} />; }
export function ClipBackendCard(props) { return <IndexTask kind="clip" {...props} />; }

function IndexTask({ kind, status, statusLoading, onRefresh, onRunIndexNow, onStopIndexNow, indexRunning, indexPhase,
  indexRetryAt, indexStopping, indexProgress, indexRun, indexError, backfill, backfillBusy, onBackfillDecision, onRetryAnn, annRetrying }) {
  const { t } = useTranslation();
  const key = 'settings.advanced.' + kind + '_backend.';
  const backend = kind === 'clip' ? status?.clip_backend : status?.backend;
  const phase = indexPhase || (indexRunning ? 'running' : 'idle');
  const running = phase === 'running' || phase === 'stopping';
  const resumable = ['waiting_for_unlock', 'waiting_for_verification', 'retry_wait'].includes(phase);
  const stoppable = running || phase === 'queued' || resumable;
  const retryAt = retryTime(indexRetryAt);
  const total = indexProgress?.total ?? 0;
  const processed = indexProgress?.processed ?? 0;
  const ratio = total > 0 ? Math.max(0, Math.min(1, processed / total)) : undefined;
  let message = t(key + 'run_now_hint');
  if (indexStopping) message = t(key + 'run_stopping');
  else if (phase === 'waiting_for_unlock') message = t(key + 'run_waiting_for_unlock');
  else if (phase === 'waiting_for_verification') message = t(key + 'run_waiting_for_verification');
  else if (phase === 'retry_wait') message = t(key + (retryAt ? 'run_retry_wait' : 'run_retry_wait_no_time'), { retryAt });
  else if (phase === 'failed') message = t(key + 'run_failed');
  else if (phase === 'queued') message = t(key + 'run_queued');
  else if (running) message = indexProgress
    ? t(key + 'run_progress', { processed, total, indexed: indexProgress.indexed ?? 0 }) : t(key + 'run_preparing');
  else if (indexRun && (!indexRun.queued || indexPhase == null)) message = indexRun.queued ? t(key + 'run_queued') : indexRun.started
    ? t(key + 'run_done', { indexed: indexRun.indexed ?? 0, remaining: indexRun.remaining ?? 0 })
    : t(key + 'run_skipped', { reason: indexRun.skipped_reason || 'unknown' });
  const annKey = { armed: 'ann_ready', arming: 'ann_building', exact_fallback: 'ann_exact', failed: 'ann_failed', disabled: 'ann_disabled', unavailable: 'ann_unavailable' }[backend?.ann_state];
  const unhealthy = kind === 'clip' && backend?.ann_build_state && backend.ann_build_state !== 'healthy';

  return (
    <section className="space-y-3">
      <SettingsRow label={t(key + 'title')} description={message}
        control={<div className="flex items-center gap-2">
          {(!stoppable || resumable) && <SettingsButton onClick={onRunIndexNow} disabled={indexStopping || (statusLoading && !backend)}>
            {t(key + (phase === 'waiting_for_unlock' ? 'run_unlock' : phase === 'waiting_for_verification' ? 'run_verify' : phase === 'failed' || phase === 'retry_wait' ? 'run_retry' : 'run_now'))}
          </SettingsButton>}
          {stoppable && <SettingsButton onClick={onStopIndexNow} disabled={indexStopping}>{t(key + (indexStopping ? 'run_stop_pending' : 'run_stop'))}</SettingsButton>}
        </div>} />
      {(running || (stoppable && total > 0)) && <progress max={1} value={ratio} aria-label={message} className="block h-1.5 w-full accent-ide-accent" />}
      {indexError && <p role="alert" className="text-xs text-ide-error">{indexError}</p>}
      {(backend?.index_stalled ?? 0) > 0 && <SettingsStatus tone="warning">{t(key + 'stalled_hint')}</SettingsStatus>}
      {unhealthy && <SettingsWarningBanner title={t(key + (backend.ann_build_state === 'circuit_open' ? 'ann_circuit_open' : 'ann_backoff'))}>
        <p>{t(key + 'ann_search_still_available')}</p>
        <p className="mt-1 break-words">{t(key + 'ann_failure_detail', { count: backend.ann_build_failure_count ?? 0, code: backend.ann_build_error_code || 'unknown', retryAt: retryTime(backend.ann_build_next_retry_at) || '—' })}</p>
        <SettingsButton className="mt-2" onClick={onRetryAnn} disabled={annRetrying}>{t(key + (annRetrying ? 'ann_retrying' : 'ann_retry_now'))}</SettingsButton>
      </SettingsWarningBanner>}
      {kind === 'clip' && backfill?.migration_settled && (backfill.never_indexed > 0 || backfill.decision) && (
        <SettingsRow label={t('clipBackfill.title')} description={backfill.decision === 'approved' ? t('clipBackfill.cardApproved')
          : backfill.decision === 'declined' ? t('clipBackfill.cardDeclined')
            : t('clipBackfill.cardPending', { count: backfill.never_indexed, estimate: formatEstimate(t, backfill.estimated_seconds) ?? '—' })}
          control={<SettingsButton disabled={backfillBusy} onClick={() => onBackfillDecision(backfill.decision === 'approved' ? 'declined' : 'approved')}>
            {t(backfill.decision === 'approved' ? 'clipBackfill.cardDecline' : 'clipBackfill.cardApprove')}
          </SettingsButton>} />
      )}
      <SettingsDisclosure title={t(key + 'diagnostic')}>
        <DiagnosticValues rows={[
          [t(key + 'indexed'), backend?.indexed_vectors],
          [t(key + 'backlog'), backend?.index_backlog],
          [t(key + 'stalled'), backend?.index_stalled],
          [t(key + 'failures'), backend?.failure_count],
          ...(kind === 'clip' ? [[t(key + 'ann_status'), annKey ? t(key + annKey) : '—'], [t(key + 'ann_generation'), backend?.ann_generation]] : []),
        ]} />
        {(backend?.last_error || backend?.ann_last_error) && <p className="break-words text-xs text-ide-muted">{backend.last_error || backend.ann_last_error}</p>}
        <SettingsButton variant="ghost" icon={RefreshCw} onClick={onRefresh} disabled={statusLoading}>{t(key + 'refresh')}</SettingsButton>
      </SettingsDisclosure>
    </section>
  );
}
