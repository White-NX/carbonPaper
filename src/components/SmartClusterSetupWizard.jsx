import React, { useState, useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { Sparkles, Download, Loader2, Info, AlertCircle, RotateCcw, CheckCircle2 } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { withAuth } from '../lib/auth_api';

/**
 * Smart Cluster Setup Wizard.
 *
 * Three exits:
 *   - "下载并启用" → download model via existing pipeline, then mark setup done
 *   - "稍后" → close for this session; wizard re-appears next launch
 *   - "不再提醒" → mark dismissed permanently; feature page still offers manual download
 *
 * Props:
 *   isVisible — whether the wizard should be rendered
 *   onComplete — (enabled: boolean) => void; enabled=true if download succeeded
 */
export default function SmartClusterSetupWizard({ isVisible, onComplete }) {
  const { t } = useTranslation();
  const [downloading, setDownloading] = useState(false);
  const [downloadLog, setDownloadLog] = useState([]);
  const [downloadError, setDownloadError] = useState(null);
  const [downloadSucceeded, setDownloadSucceeded] = useState(false);
  const logRef = useRef(null);
  const startedRef = useRef(false);

  // Capture install-log events into our local panel only while downloading.
  useEffect(() => {
    if (!downloading) return;
    let mounted = true;
    let unlisten;
    (async () => {
      try {
        unlisten = await listen('install-log', (event) => {
          if (!mounted) return;
          const payload = event?.payload || {};
          const line = payload.line || JSON.stringify(payload);
          const ts = new Date().toLocaleTimeString();
          setDownloadLog((prev) => [...prev, `[${ts}] ${line}`]);
        });
      } catch (e) {
        console.warn('Failed to register install-log listener', e);
      }
    })();
    return () => {
      mounted = false;
      if (unlisten) unlisten();
    };
  }, [downloading]);

  // Auto-scroll log
  useEffect(() => {
    if (logRef.current) {
      logRef.current.scrollTop = logRef.current.scrollHeight;
    }
  }, [downloadLog]);

  const startDownload = async () => {
    if (startedRef.current) return;
    startedRef.current = true;
    setDownloading(true);
    setDownloadLog([]);
    setDownloadError(null);
    setDownloadSucceeded(false);

    let success = false;
    try {
      setDownloadLog((prev) => [...prev, `[${new Date().toLocaleTimeString()}] ${t('smartClusterSetup.downloading', 'Downloading bge-reranker-v2-m3 (uint8, ~570MB)…')}`]);
      await invoke('download_model', {
        repo: 'onnx-community/bge-reranker-v2-m3-ONNX',
        subdir: 'bge-reranker-v2-m3',
        files: [
          'config.json',
          'tokenizer.json',
          'tokenizer_config.json',
          'special_tokens_map.json',
          'onnx/model_uint8.onnx',
        ],
      });
      setDownloadLog((prev) => [...prev, `[${new Date().toLocaleTimeString()}] ${t('smartClusterSetup.downloadComplete', 'Download complete')}`]);
      // Mark setup done (non-dismissed) and enable the feature.
      await invoke('mark_smart_cluster_setup_done', { dismissedPermanently: false });
      await withAuth(() => invoke('set_advanced_config', { config: { smart_cluster_enabled: true } }), { autoPrompt: true });
      setDownloadSucceeded(true);
      success = true;
    } catch (err) {
      setDownloadError(err?.message || String(err));
    } finally {
      setDownloading(false);
      if (!success) {
        startedRef.current = false;
      }
    }
  };

  const handleDismissPermanently = async () => {
    try {
      await invoke('mark_smart_cluster_setup_done', { dismissedPermanently: true });
    } catch (err) {
      console.warn('Failed to mark smart cluster setup dismissed:', err);
    }
    onComplete?.(false);
  };

  const handleLater = () => {
    // Don't write any flag — wizard will reappear next launch.
    onComplete?.(false);
  };

  const handleFinish = () => {
    onComplete?.(true);
  };

  if (!isVisible) return null;

  return (
    <div className="absolute inset-0 z-50 flex flex-col items-center justify-center bg-ide-bg/80 backdrop-blur-sm text-ide-muted">
      <div className="w-full max-w-xl bg-ide-panel border border-ide-border rounded-xl p-6 shadow-2xl">
        {/* Header */}
        <div className="flex items-center gap-3 mb-1">
          <div className="w-10 h-10 rounded-lg bg-ide-bg border border-ide-border flex items-center justify-center">
            <Sparkles className="w-5 h-5 text-ide-accent" />
          </div>
          <div>
            <h2 className="text-lg font-semibold text-ide-text">
              {t('smartClusterSetup.title', '智能聚类（新功能）')}
            </h2>
            <p className="text-xs text-ide-muted">
              {t('smartClusterSetup.subtitle', '按自然语言描述自动归档相关快照')}
            </p>
          </div>
        </div>

        {/* Description */}
        <div className="mt-4 space-y-3">
          <p className="text-sm text-ide-text/90 leading-relaxed">
            {t('smartClusterSetup.description', '输入一句话（例如 "对加利福尼亚地区山脉的研究"），CarbonPaper 会自动把相关快照归到这个分类下——既包括历史快照也包括之后新拍下的。')}
          </p>

          <div className="flex items-start gap-2 bg-ide-bg rounded-lg border border-ide-border p-3">
            <Download className="w-4 h-4 text-ide-accent shrink-0 mt-0.5" />
            <div className="text-xs text-ide-muted leading-relaxed">
              <p>{t('smartClusterSetup.modelInfo', '需要下载 bge-reranker-v2-m3 (uint8 ONNX)，约 570MB。模型仅在系统空闲时使用，不会影响前台应用。')}</p>
              <p className="opacity-70 mt-1">
                {t('smartClusterSetup.modelSource', '来源: huggingface.co/onnx-community/bge-reranker-v2-m3-ONNX')}
              </p>
            </div>
          </div>

          <div className="flex items-start gap-2 px-1">
            <Info className="w-3.5 h-3.5 text-ide-muted/60 shrink-0 mt-0.5" />
            <p className="text-xs text-ide-muted/70 leading-relaxed">
              {t('smartClusterSetup.infoNote', '可以选择"不再提醒"——之后随时可以在设置「功能管理」里手动下载并启用。')}
            </p>
          </div>
        </div>

        {/* Download log (only while downloading or after error) */}
        {(downloading || downloadError || downloadSucceeded) && (
          <div className="mt-4">
            <textarea
              ref={logRef}
              readOnly
              value={downloadLog.join('\n')}
              rows={8}
              className={`w-full bg-ide-bg border ${downloadError ? 'border-rose-400' : 'border-ide-border'} rounded-md p-3 text-xs font-mono ${downloadError ? 'text-rose-400' : 'text-ide-muted'} resize-none`}
            />
            {downloadError && (
              <div className="mt-2 flex items-center gap-3">
                <AlertCircle className="w-3.5 h-3.5 text-rose-400 shrink-0" />
                <span className="text-xs text-rose-400 flex-1 break-all">{downloadError}</span>
                <button
                  onClick={startDownload}
                  className="flex items-center gap-1 px-2 py-1 bg-blue-600 hover:bg-blue-700 text-white rounded text-xs transition-colors"
                >
                  <RotateCcw className="w-3 h-3" />
                  {t('smartClusterSetup.retry', '重试')}
                </button>
              </div>
            )}
            {downloadSucceeded && (
              <div className="mt-2 flex items-center gap-2 text-xs text-emerald-400">
                <CheckCircle2 className="w-3.5 h-3.5 shrink-0" />
                {t('smartClusterSetup.success', '模型下载完成，功能已启用')}
              </div>
            )}
          </div>
        )}

        {/* Actions */}
        <div className="mt-5 flex items-center justify-between gap-2">
          {!downloadSucceeded ? (
            <>
              <button
                onClick={handleDismissPermanently}
                disabled={downloading}
                className="px-3 py-1.5 text-xs text-ide-muted hover:text-rose-400 transition-colors disabled:opacity-50"
                title={t('smartClusterSetup.dismissTooltip', '不再弹出此提示。可以在设置里手动启用。')}
              >
                {t('smartClusterSetup.dismiss', '不再提醒')}
              </button>
              <div className="flex items-center gap-2">
                <button
                  onClick={handleLater}
                  disabled={downloading}
                  className="px-4 py-1.5 bg-ide-bg hover:bg-ide-bg/80 text-ide-muted border border-ide-border rounded text-sm transition-colors disabled:opacity-50"
                >
                  {t('smartClusterSetup.later', '稍后')}
                </button>
                <button
                  onClick={startDownload}
                  disabled={downloading}
                  className="px-4 py-1.5 bg-ide-accent hover:bg-ide-accent/90 text-white rounded text-sm font-medium transition-colors disabled:opacity-50 flex items-center gap-1.5"
                >
                  {downloading ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : <Download className="w-3.5 h-3.5" />}
                  {downloading ? t('smartClusterSetup.downloading_short', '下载中…') : t('smartClusterSetup.download', '下载并启用')}
                </button>
              </div>
            </>
          ) : (
            <button
              onClick={handleFinish}
              className="ml-auto px-4 py-1.5 bg-ide-accent hover:bg-ide-accent/90 text-white rounded text-sm font-medium transition-colors"
            >
              {t('smartClusterSetup.openFeature', '打开智能聚类')}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
