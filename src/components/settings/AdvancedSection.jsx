import React, { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Activity, Cpu, RefreshCw, Wrench } from 'lucide-react';
import { SettingsButton, SettingsSelect, SettingsSwitch } from './SettingsControls';
import { SettingsDisclosure, SettingsDivider, SettingsGroup, SettingsRow, SettingsSection, SettingsStatus } from './SettingsPrimitives';
import { OcrEngineCard, DiagnosticValues } from './advanced/InferenceCards';
import { CPU_PERCENT_OPTIONS } from './advanced/advancedOptions';
import ModelInventoryTable from './organize/ModelInventoryTable';
import DeveloperTools from './advanced/DeveloperTools';
import { useModelInventory } from './organize/useModelInventory';
import { useSettingsActive, useSettingsActivity } from './SettingsActivityContext';

function OcrTimeoutRow({ value, onChange, disabled }) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState(String(value ?? 120));
  const [error, setError] = useState(false);
  useEffect(() => { setDraft(String(value ?? 120)); }, [value]);
  useSettingsActivity('ocr-timeout', { dirty: draft !== String(value ?? 120) });
  const commit = async () => {
    if (draft === String(value ?? 120)) return;
    const valid = Number.isInteger(Number(draft)) && Number(draft) >= 30 && Number(draft) <= 600;
    setError(!valid);
    if (valid && await onChange(Number(draft))) setError(false);
  };
  return <SettingsRow label={t('settings.advanced.ocr.timeout_label')} description={t('settings.advanced.ocr.timeout_desc')}
    control={<div className="flex items-center gap-2">
      <input type="number" min={30} max={600} step={10} value={draft} disabled={disabled} aria-label={t('settings.advanced.ocr.timeout_label')}
        aria-invalid={error} className="w-24 rounded-lg border border-ide-border bg-ide-panel px-3 py-2 text-right text-sm"
        onChange={(event) => setDraft(event.target.value)} onBlur={commit}
        onKeyDown={(event) => { if (event.key === 'Enter') event.currentTarget.blur(); if (event.key === 'Escape') { setDraft(String(value)); setError(false); } }} />
      <span className="text-xs text-ide-muted">{t('settings.advanced.ocr.seconds')}</span>
    </div>}>{error && <SettingsStatus tone="error">{t('settings.advanced.ocr.invalidTimeout')}</SettingsStatus>}</SettingsRow>;
}

export default function AdvancedSection({ controller: r }) {
  const { t } = useTranslation();
  const active = useSettingsActive();
  const [modelsOpen, setModelsOpen] = useState(false);
  const inventory = useModelInventory(active && modelsOpen);
  const config = r.config;
  if (!config) return <SettingsStatus>{t(r.configError ? 'settings.feedback.readFailed' : 'settings.feedback.loading')}</SettingsStatus>;
  const gpuOptions = r.gpus.map((gpu) => ({ value: gpu.id, label: gpu.name }));
  if (!r.selectedGpu) gpuOptions.unshift({ value: config.dml_device_id, label: t('settings.advanced.dml.selectDevice'), disabled: true });
  const classification = r.semanticStatus?.classification_backend;
  const scheduler = r.backgroundSchedulerStatus;
  return <div className="space-y-6">
    <SettingsSection title={t('settings.advanced.groups.performance')} icon={Cpu}>
      <SettingsGroup>
        <SettingsRow label={t('settings.advanced.cpu.label')} description={t('settings.advanced.cpu.description')}
          control={<SettingsSwitch checked={config.cpu_limit_enabled} disabled={r.configSaving} onChange={() => r.handleToggle('cpu_limit_enabled')} />}>
          {config.cpu_limit_enabled && <SettingsSelect label={t('settings.advanced.cpu.percent_label')} value={config.cpu_limit_percent} disabled={r.configSaving}
            options={CPU_PERCENT_OPTIONS.map((value) => ({ value, label: value + '%' }))} onChange={r.handleCpuPercentChange} />}
        </SettingsRow>
        <SettingsDivider />
        <SettingsRow label={t('settings.advanced.dml.label')} description={t('settings.advanced.dml.description')}
          control={<SettingsSwitch checked={config.use_dml} disabled={r.configSaving} onChange={() => r.handleToggle('use_dml')} />}>
          {config.use_dml && (r.gpuLoading ? <SettingsStatus>{t('settings.advanced.dml.gpu_loading')}</SettingsStatus>
            : r.gpus.length ? <SettingsSelect label={t('settings.advanced.dml.gpu_select')} value={config.dml_device_id} options={gpuOptions} disabled={r.configSaving} onChange={r.handleGpuChange} />
              : <SettingsStatus>{t('settings.advanced.dml.gpu_none')}</SettingsStatus>)}
        </SettingsRow>
        <SettingsDivider />
        <OcrTimeoutRow value={config.ocr_timeout_secs} disabled={r.configSaving} onChange={r.handleOcrTimeoutChange} />
        <SettingsDivider />
        <SettingsRow label={t('settings.advanced.clustering.allow_full_low_memory_label')} description={t('settings.advanced.clustering.allow_full_low_memory_desc')}
          control={<SettingsSwitch checked={config.clustering_allow_full_low_memory} disabled={r.configSaving} onChange={() => r.handleToggle('clustering_allow_full_low_memory')} />} />
      </SettingsGroup>
    </SettingsSection>
    <SettingsSection title={t('settings.advanced.groups.components')} icon={Wrench}>
      <OcrEngineCard status={r.mlOcrStatus} statusLoading={r.mlOcrStatusLoading} modelStatus={r.rustOcrModelStatus}
        modelDownloading={r.rustOcrModelDownloading} onRestart={r.handleRestartMlOcr} onDownloadModel={r.handleDownloadRustOcrModel}
        onRefresh={r.refreshRustOcrModelStatus} error={r.runtimeError} />
      <SettingsDisclosure title={t('settings.features.models.title')} open={modelsOpen} onOpenChange={setModelsOpen}>
        <ModelInventoryTable models={inventory.models} modelsLoading={inventory.modelsLoading} onRefresh={inventory.loadModels} onOpenLocation={inventory.handleOpenLocation} formatSize={inventory.formatSize} />
      </SettingsDisclosure>
    </SettingsSection>
    <SettingsSection title={t('settings.advanced.groups.diagnostics')} icon={Activity}>
      <SettingsGroup>
        <SettingsDisclosure title={t('settings.advanced.background_processing.diagnostics')}>
          <DiagnosticValues rows={[
            [t('settings.advanced.background_processing.profile'), scheduler?.execution_profile],
            [t('settings.advanced.background_processing.waitReason'), scheduler?.blocked_reason],
            [t('settings.advanced.background_processing.pauses'), scheduler?.pauses?.total],
            [t('settings.advanced.background_processing.yieldLatency'), scheduler?.pauses?.last_yield_ms == null ? '—' : Math.round(scheduler.pauses.last_yield_ms) + ' ms'],
          ]} />
          <SettingsButton variant="ghost" icon={RefreshCw} onClick={r.refreshBackgroundSchedulerStatus}>{t('common.refresh')}</SettingsButton>
        </SettingsDisclosure>
        <SettingsDivider />
        <SettingsDisclosure title={t('settings.advanced.classification_backend.title')}>
          <DiagnosticValues rows={[
            [t('settings.advanced.classification_backend.successes'), classification?.success_count],
            [t('settings.advanced.classification_backend.failures'), classification?.failure_count],
            [t('settings.advanced.classification_backend.last_elapsed'), classification?.last_elapsed_ms == null ? '—' : Math.round(classification.last_elapsed_ms) + ' ms'],
          ]} />
          {classification?.last_error && <p className="break-words text-xs text-ide-muted">{classification.last_error}</p>}
          <SettingsButton variant="ghost" icon={RefreshCw} onClick={r.refreshSemanticStatus}>{t('common.refresh')}</SettingsButton>
        </SettingsDisclosure>
      </SettingsGroup>
    </SettingsSection>
    {import.meta.env.DEV && <DeveloperTools />}
  </div>;
}
