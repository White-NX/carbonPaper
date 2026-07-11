import React from 'react';
import { useTranslation } from 'react-i18next';
import { ChevronDown, Clock, Info, ListOrdered } from 'lucide-react';
import { SettingsSwitch } from '../SettingsControls';
import { OCR_QUEUE_SIZE_OPTIONS } from './advancedOptions';

export default function OcrQueueCard({
  config,
  queueDropdownOpen,
  onToggle,
  onToggleQueueDropdown,
  onQueueSizeChange,
  onOcrTimeoutDraftChange,
  onOcrTimeoutChange,
}) {
  const { t } = useTranslation();

  return (
    <div className="space-y-3">
      <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
        <ListOrdered className="w-4 h-4" />
        {t('settings.advanced.ocr.title')}
      </label>

      <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
        <div className="flex items-center justify-between gap-4">
          <div className="flex-1 min-w-0">
            <p className="text-sm text-ide-text font-medium">{t('settings.advanced.ocr.pause_label')}</p>
            <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.ocr.pause_desc')}</p>
          </div>
          <SettingsSwitch
            checked={!config.capture_on_ocr_busy}
            onChange={() => onToggle('capture_on_ocr_busy')}
          />
        </div>

        <div className="flex items-center justify-between gap-4">
          <div className="flex-1 min-w-0">
            <p className="text-sm text-ide-text font-medium">{t('settings.advanced.ocr.queue_limit_label')}</p>
            <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.ocr.queue_limit_desc')}</p>
          </div>
          <SettingsSwitch
            checked={config.ocr_queue_limit_enabled}
            onChange={() => onToggle('ocr_queue_limit_enabled')}
          />
        </div>

        {config.ocr_queue_limit_enabled && (
          <div className="flex items-center justify-between gap-4">
            <p className="text-sm text-ide-muted">{t('settings.advanced.ocr.max_queue_label')}</p>
            <div className="relative">
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onToggleQueueDropdown();
                }}
                className="flex items-center gap-2 px-4 py-2 bg-ide-panel border border-ide-border rounded-lg text-sm text-ide-text hover:bg-ide-hover transition-colors min-w-[100px]"
              >
                <span className="flex-1 text-left">{config.ocr_queue_max_size}</span>
                <ChevronDown
                  className={`w-4 h-4 text-ide-muted transition-transform ${queueDropdownOpen ? 'rotate-180' : ''}`}
                />
              </button>
              {queueDropdownOpen && (
                <div
                  className="absolute right-0 top-full mt-2 w-32 bg-ide-panel border border-ide-border rounded-xl shadow-xl z-50 overflow-hidden"
                  onClick={(e) => e.stopPropagation()}
                >
                  {OCR_QUEUE_SIZE_OPTIONS.map((size) => (
                    <button
                      key={size}
                      onClick={() => onQueueSizeChange(size)}
                      className={`w-full px-4 py-2.5 text-left hover:bg-ide-hover transition-colors flex items-center justify-between ${size === config.ocr_queue_max_size ? 'bg-ide-accent/10' : ''}`}
                    >
                      <span className="text-sm text-ide-text">{size}</span>
                      {size === config.ocr_queue_max_size && (
                        <div className="w-2 h-2 rounded-full bg-ide-accent shrink-0" />
                      )}
                    </button>
                  ))}
                </div>
              )}
            </div>
          </div>
        )}

        <div className="flex items-center justify-between gap-4">
          <div className="flex-1 min-w-0">
            <p className="text-sm text-ide-text font-medium flex items-center gap-2">
              <Clock className="w-4 h-4 text-ide-muted" />
              {t('settings.advanced.ocr.timeout_label', 'OCR 超时时间')}
            </p>
            <p className="text-xs text-ide-muted mt-1">
              {t('settings.advanced.ocr.timeout_desc', '设定 OCR 任务的超时时间。冷启动固定允许 180 秒。')}
            </p>
          </div>
          <div className="flex items-center gap-2 shrink-0">
            <input
              type="number"
              min="30"
              max="600"
              step="10"
              value={config.ocr_timeout_secs || 120}
              onChange={(e) => onOcrTimeoutDraftChange(e.target.value)}
              onBlur={(e) => onOcrTimeoutChange(e.target.value)}
              className="w-24 px-3 py-2 bg-ide-panel border border-ide-border rounded-lg text-sm text-ide-text text-right"
            />
            <span className="text-xs text-ide-muted">{t('settings.advanced.ocr.seconds', '秒')}</span>
          </div>
        </div>

        <div className="flex items-start gap-2 p-2.5 bg-ide-panel/50 border border-ide-border/30 rounded-lg">
          <Info className="w-4 h-4 text-ide-muted shrink-0 mt-0.5" />
          <p className="text-xs text-ide-muted leading-relaxed">{t('settings.advanced.ocr.info')}</p>
        </div>
      </div>
    </div>
  );
}
