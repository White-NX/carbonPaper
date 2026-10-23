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

export default function AdvancedSection({ preferences: p, ocr, indexes, scheduler }) {
  const { t } = useTranslation();
  const active = useSettingsActive();
  const [modelsOpen, setModelsOpen] = useState(false);
  const inventory = useModelInventory(active && modelsOpen);
  const config = p.config;
  if (!config) return <SettingsStatus>{t(p.configError ? 'settings.feedback.readFailed' : 'settings.feedback.loading')}</SettingsStatus>;
  const gpuOptions = p.gpus.map((gpu) => ({ value: gpu.id, label: gpu.name }));
  if (!p.selectedGpu) gpuOptions.unshift({ value: config.dml_device_id, label: t('settings.advanced.dml.selectDevice'), disabled: true });
  const classification = indexes.status?.classification_backend;
  return <div className="space-y-6">
    <SettingsSection title={t('settings.advanced.groups.performance')} icon={Cpu}>
      <SettingsGroup>
        <SettingsRow label={t('settings.advanced.cpu.label')} description={t('settings.advanced.cpu.description')}
          control={<SettingsSwitch checked={config.cpu_limit_enabled} disabled={p.configSaving} onChange={() => p.handleToggle('cpu_limit_enabled')} />}>
          {config.cpu_limit_enabled && <SettingsSelect label={t('settings.advanced.cpu.percent_label')} value={config.cpu_limit_percent} disabled={p.configSaving}
            options={CPU_PERCENT_OPTIONS.map((value) => ({ value, label: value + '%' }))} onChange={p.handleCpuPercentChange} />}
        </SettingsRow>
        <SettingsDivider />
        <SettingsRow label={t('settings.advanced.dml.label')} description={t('settings.advanced.dml.description')}
          control={<SettingsSwitch checked={config.use_dml} disabled={p.configSaving} onChange={() => p.handleToggle('use_dml')} />}>
          {config.use_dml && (p.gpuLoading ? <SettingsStatus>{t('settings.advanced.dml.gpu_loading')}</SettingsStatus>
            : p.gpus.length ? <SettingsSelect label={t('settings.advanced.dml.gpu_select')} value={config.dml_device_id} options={gpuOptions} disabled={p.configSaving} onChange={p.handleGpuChange} />
              : <SettingsStatus>{t('settings.advanced.dml.gpu_none')}</SettingsStatus>)}
        </SettingsRow>
        <SettingsDivider />
        <OcrTimeoutRow value={config.ocr_timeout_secs} disabled={p.configSaving} onChange={p.handleOcrTimeoutChange} />
      </SettingsGroup>
    </SettingsSection>
    <SettingsSection title={t('settings.advanced.groups.components')} icon={Wrench}>
      <OcrEngineCard status={ocr.status} statusLoading={ocr.statusLoading} modelStatus={ocr.modelStatus}
        modelDownloading={ocr.modelDownloading} onRestart={ocr.restart} onDownloadModel={ocr.downloadModel}
        onRefresh={ocr.refreshModel} error={ocr.error} />
      <SettingsDisclosure title={t('settings.features.models.title')} open={modelsOpen} onOpenChange={setModelsOpen}>
        <ModelInventoryTable models={inventory.models} modelsLoading={inventory.modelsLoading} onRefresh={inventory.loadModels} onOpenLocation={inventory.handleOpenLocation} formatSize={inventory.formatSize} />
      </SettingsDisclosure>
    </SettingsSection>
    <SettingsSection title={t('settings.advanced.groups.diagnostics')} icon={Activity}>
      <SettingsGroup>
        <SettingsDisclosure title={t('settings.advanced.background_processing.diagnostics')}>
          <DiagnosticValues rows={[
            [t('settings.advanced.background_processing.profile'), scheduler.status?.execution_profile],
            [t('settings.advanced.background_processing.waitReason'), scheduler.status?.blocked_reason],
            [t('settings.advanced.background_processing.pauses'), scheduler.status?.pauses?.total],
            [t('settings.advanced.background_processing.yieldLatency'), scheduler.status?.pauses?.last_yield_ms == null ? '—' : Math.round(scheduler.status.pauses.last_yield_ms) + ' ms'],
          ]} />
          <SettingsButton variant="ghost" icon={RefreshCw} onClick={scheduler.refresh}>{t('common.refresh')}</SettingsButton>
        </SettingsDisclosure>
        <SettingsDivider />
        <SettingsDisclosure title={t('settings.advanced.classification_backend.title')}>
          <DiagnosticValues rows={[
            [t('settings.advanced.classification_backend.successes'), classification?.success_count],
            [t('settings.advanced.classification_backend.failures'), classification?.failure_count],
            [t('settings.advanced.classification_backend.last_elapsed'), classification?.last_elapsed_ms == null ? '—' : Math.round(classification.last_elapsed_ms) + ' ms'],
          ]} />
          {classification?.last_error && <p className="break-words text-xs text-ide-muted">{classification.last_error}</p>}
          <SettingsButton variant="ghost" icon={RefreshCw} onClick={indexes.refresh}>{t('common.refresh')}</SettingsButton>
        </SettingsDisclosure>
      </SettingsGroup>
    </SettingsSection>
    {import.meta.env.DEV && <DeveloperTools />}
  </div>;
}
