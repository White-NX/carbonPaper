import React from 'react';
import { useTranslation } from 'react-i18next';
import { Clock, Info, ListOrdered } from 'lucide-react';

export default function OcrQueueCard({
  config,
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
        <div className="flex items-start gap-3 rounded-lg border border-ide-border/60 bg-ide-panel/40 p-3">
          <Info className="w-4 h-4 text-ide-accent shrink-0 mt-0.5" />
          <div>
            <p className="text-sm text-ide-text font-medium">
              {t('settings.advanced.ocr.single_flight', '固定单任务模式')}
            </p>
            <p className="text-xs text-ide-muted mt-1 leading-relaxed">
              {t(
                'settings.advanced.ocr.single_flight_desc',
                'OCR 直接处理未经过 JPEG 的 RGB 捕获帧。为限制内存占用，OCR 运行期间不会排队新的完整 RGB 帧。',
              )}
            </p>
          </div>
        </div>

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
      </div>
    </div>
  );
}
