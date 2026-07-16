import React, { useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { AlertTriangle, CheckCircle2, Download, Loader2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Dialog } from './Dialog';
import { useTauriEventListener } from '../hooks/useTauriEventListener';

export default function OcrModelRepairCard({ isOpen, onClose }) {
  const { t } = useTranslation();
  const [modelStatus, setModelStatus] = useState(null);
  const [checking, setChecking] = useState(false);
  const [repairing, setRepairing] = useState(false);
  const [progress, setProgress] = useState(null);
  const [error, setError] = useState('');

  useTauriEventListener('rust-ocr-model-download-progress', (event) => {
    if (repairing) setProgress(event.payload || null);
  }, [repairing], isOpen);

  useEffect(() => {
    if (!isOpen) return undefined;
    let active = true;
    setChecking(true);
    setError('');
    invoke('get_rust_ocr_model_status')
      .then((status) => {
        if (active) setModelStatus(status);
      })
      .catch((err) => {
        if (active) setError(err?.message || String(err));
      })
      .finally(() => {
        if (active) setChecking(false);
      });
    return () => {
      active = false;
    };
  }, [isOpen]);

  const percent = useMemo(() => {
    const downloaded = Number(progress?.downloaded);
    const total = Number(progress?.total);
    if (!Number.isFinite(downloaded) || !Number.isFinite(total) || total <= 0) return null;
    return Math.min(100, Math.max(0, Math.round((downloaded / total) * 100)));
  }, [progress]);

  const handleRepair = async () => {
    setRepairing(true);
    setProgress(null);
    setError('');
    try {
      const status = await invoke('download_rust_ocr_model');
      setModelStatus(status);
    } catch (err) {
      setError(err?.message || String(err));
    } finally {
      setRepairing(false);
    }
  };

  const installed = modelStatus?.installed === true;

  return (
    <Dialog
      isOpen={isOpen}
      onClose={repairing ? () => {} : onClose}
      disableClose={repairing}
      hideCloseButton={repairing}
      title={t('ocrModelRepair.title', 'OCR 模型修复')}
      maxWidth="max-w-xl"
    >
      <div className="p-6 space-y-5">
        <div className={`rounded-xl border p-4 ${installed ? 'border-emerald-500/40 bg-emerald-500/10' : 'border-amber-500/50 bg-amber-500/10'}`}>
          <div className="flex items-start gap-3">
            {installed
              ? <CheckCircle2 className="w-7 h-7 text-emerald-400 shrink-0" />
              : <AlertTriangle className="w-7 h-7 text-amber-400 shrink-0" />}
            <div>
              <h2 className="text-base font-semibold text-ide-text">
                {installed
                  ? t('ocrModelRepair.ready', 'OCR 模型已就绪')
                  : t('ocrModelRepair.missing', 'OCR 当前不可用')}
              </h2>
              <p className="text-sm text-ide-muted mt-2 leading-relaxed">
                {installed
                  ? t('ocrModelRepair.readyDescription', '新的截图将恢复生成 OCR 文本。')
                  : t('ocrModelRepair.description', 'CarbonPaper 仍会安全保存截图，但在模型修复前不会生成 OCR 文本，也不会回退到 JPEG 或 Python OCR。')}
              </p>
            </div>
          </div>
        </div>

        {!installed && (
          <div className="space-y-3">
            <p className="text-sm text-ide-text">
              {t('ocrModelRepair.noAuth', '修复不需要解锁。下载完成后会校验文件大小和 SHA-256，下一次 OCR 会自动载入新模型。')}
            </p>
            {modelStatus?.path && (
              <p className="text-xs text-ide-muted break-all">{modelStatus.path}</p>
            )}
            {repairing && (
              <div className="space-y-2">
                <div className="flex justify-between text-xs text-ide-muted">
                  <span>{progress?.file || t('ocrModelRepair.preparing', '正在准备下载…')}</span>
                  <span>
                    {progress?.asset_index && progress?.asset_count
                      ? `${progress.asset_index}/${progress.asset_count}${percent == null ? '' : ` · ${percent}%`}`
                      : ''}
                  </span>
                </div>
                <div className="h-2 rounded-full bg-ide-panel overflow-hidden">
                  <div
                    className={`h-full bg-ide-accent transition-all ${percent == null ? 'w-1/3 animate-pulse' : ''}`}
                    style={percent == null ? undefined : { width: `${percent}%` }}
                  />
                </div>
              </div>
            )}
          </div>
        )}

        {checking && (
          <div className="flex items-center gap-2 text-sm text-ide-muted">
            <Loader2 className="w-4 h-4 animate-spin" />
            {t('ocrModelRepair.checking', '正在检查模型状态…')}
          </div>
        )}

        {error && (
          <div className="rounded-lg border border-red-500/40 bg-red-500/10 p-3 text-sm text-red-300 break-words">
            {error}
          </div>
        )}

        <div className="flex justify-end gap-3">
          <button
            onClick={onClose}
            disabled={repairing}
            className="px-4 py-2 rounded border border-ide-border text-sm text-ide-text hover:bg-ide-hover disabled:opacity-50"
          >
            {installed ? t('ocrModelRepair.close', '关闭') : t('ocrModelRepair.later', '稍后处理')}
          </button>
          {!installed && (
            <button
              onClick={handleRepair}
              disabled={repairing || checking}
              className="px-4 py-2 rounded bg-ide-accent text-white text-sm font-medium hover:opacity-90 disabled:opacity-50 flex items-center gap-2"
            >
              {repairing
                ? <Loader2 className="w-4 h-4 animate-spin" />
                : <Download className="w-4 h-4" />}
              {repairing
                ? t('ocrModelRepair.repairing', '正在修复…')
                : t('ocrModelRepair.repair', '立即修复模型')}
            </button>
          )}
        </div>
      </div>
    </Dialog>
  );
}
