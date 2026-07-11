import React from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, ChevronDown, Monitor, Zap } from 'lucide-react';
import SettingsHelpTooltip from '../SettingsHelpTooltip';
import { SettingsSwitch } from '../SettingsControls';

function ChangedNotice({
  children,
  monitorStatus,
  onRestartMonitor,
  onClearChanged,
}) {
  const { t } = useTranslation();

  return (
    <div className="flex items-center gap-2 p-2.5 bg-ide-warning-bg border border-ide-warning-border rounded-lg">
      <AlertTriangle className="w-4 h-4 text-ide-warning shrink-0" />
      <p className="text-xs text-ide-warning-muted flex-1">{children}</p>
      {monitorStatus === 'running' && onRestartMonitor && (
        <button
          onClick={() => { onRestartMonitor(); onClearChanged(); }}
          className="text-xs text-ide-warning hover:opacity-80 underline shrink-0 transition-colors"
        >
          {t('settings.advanced.quick_restart')}
        </button>
      )}
    </div>
  );
}

export function DmlAccelerationCard({
  config,
  monitorStatus,
  dmlChanged,
  gpus,
  gpuLoading,
  selectedGpu,
  gpuDropdownOpen,
  onToggle,
  onToggleGpuDropdown,
  onGpuChange,
  onRestartMonitor,
  onClearChanged,
}) {
  const { t } = useTranslation();

  return (
    <div className="space-y-3">
      <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
        <Zap className="w-4 h-4" />
        {t('settings.advanced.dml.title')}
      </label>

      <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
        <div className="flex items-center justify-between gap-4">
          <div className="flex-1 min-w-0">
            <p className="text-sm text-ide-text font-medium">
              {t('settings.advanced.dml.label')}
              <SettingsHelpTooltip variant="term">{t('settings.advanced.terms.directml')}</SettingsHelpTooltip>
            </p>
            <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.dml.description')}</p>
            <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.dml.notice')}</p>
          </div>
          <SettingsSwitch
            checked={config.use_dml}
            onChange={() => onToggle('use_dml')}
          />
        </div>

        {config.use_dml && (
          <div className="flex items-center justify-between gap-4">
            <div className="flex items-center gap-2">
              <Monitor className="w-4 h-4 text-ide-muted" />
              <p className="text-sm text-ide-muted">{t('settings.advanced.dml.gpu_select')}</p>
            </div>
            <div className="relative">
              {gpuLoading ? (
                <p className="text-xs text-ide-muted px-4 py-2">{t('settings.advanced.dml.gpu_loading')}</p>
              ) : gpus.length === 0 ? (
                <p className="text-xs text-ide-muted px-4 py-2">{t('settings.advanced.dml.gpu_none')}</p>
              ) : (
                <>
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      onToggleGpuDropdown();
                    }}
                    className="flex items-center gap-2 px-4 py-2 bg-ide-panel border border-ide-border rounded-lg text-sm text-ide-text hover:bg-ide-hover transition-colors min-w-[180px] max-w-[280px]"
                  >
                    <span className="flex-1 text-left truncate">{selectedGpu?.name || `GPU ${config.dml_device_id}`}</span>
                    <ChevronDown
                      className={`w-4 h-4 text-ide-muted transition-transform shrink-0 ${gpuDropdownOpen ? 'rotate-180' : ''}`}
                    />
                  </button>
                  {gpuDropdownOpen && (
                    <div
                      className="absolute right-0 top-full mt-2 w-72 bg-ide-panel border border-ide-border rounded-xl shadow-xl z-50 overflow-hidden"
                      onClick={(e) => e.stopPropagation()}
                    >
                      {gpus.map((gpu) => (
                        <button
                          key={gpu.id}
                          onClick={() => onGpuChange(gpu.id)}
                          className={`w-full px-4 py-2.5 text-left hover:bg-ide-hover transition-colors flex items-center justify-between gap-2 ${gpu.id === config.dml_device_id ? 'bg-ide-accent/10' : ''}`}
                        >
                          <span className="text-sm text-ide-text truncate">{gpu.name}</span>
                          {gpu.id === config.dml_device_id && (
                            <div className="w-2 h-2 rounded-full bg-ide-accent shrink-0" />
                          )}
                        </button>
                      ))}
                    </div>
                  )}
                </>
              )}
            </div>
          </div>
        )}

        {dmlChanged && (
          <ChangedNotice
            monitorStatus={monitorStatus}
            onRestartMonitor={onRestartMonitor}
            onClearChanged={onClearChanged}
          >
            {t('settings.advanced.dml.changed_notice')}
          </ChangedNotice>
        )}
      </div>
    </div>
  );
}

export function OnnxRuntimeCard({
  config,
  monitorStatus,
  onnxChanged,
  onToggle,
  onRestartMonitor,
  onClearChanged,
}) {
  const { t } = useTranslation();

  return (
    <div className="space-y-3">
      <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
        <Zap className="w-4 h-4" />
        {t('settings.advanced.onnx.title')}
      </label>

      <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
        <div className="flex items-center justify-between gap-4">
          <div className="flex-1 min-w-0">
            <p className="text-sm text-ide-text font-medium">
              {t('settings.advanced.onnx.label')}
              <SettingsHelpTooltip variant="term">{t('settings.advanced.terms.onnx')}</SettingsHelpTooltip>
            </p>
            <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.onnx.description')}</p>
            <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.onnx.notice')}</p>
          </div>
          <SettingsSwitch
            checked={config.use_onnx}
            onChange={() => onToggle('use_onnx')}
          />
        </div>

        {onnxChanged && (
          <ChangedNotice
            monitorStatus={monitorStatus}
            onRestartMonitor={onRestartMonitor}
            onClearChanged={onClearChanged}
          >
            {t('settings.advanced.onnx.changed_notice')}
          </ChangedNotice>
        )}
      </div>
    </div>
  );
}
