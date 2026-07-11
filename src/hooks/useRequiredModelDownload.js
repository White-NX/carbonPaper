import { useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTauriEventListener } from './useTauriEventListener';

export function useRequiredModelDownload({
  modelsNeedDownload,
  missingModels,
  renderVenvInstallStep,
  depsNeedUpdate,
  onModelsDownloadComplete,
  t,
}) {
  const [modelDownloadLog, setModelDownloadLog] = useState([]);
  const [modelDownloadError, setModelDownloadError] = useState(null);
  const [modelDownloading, setModelDownloading] = useState(false);
  const modelDownloadLogRef = useRef(null);
  const modelDownloadStartedRef = useRef(false);
  const [retryNonce, setRetryNonce] = useState(0);
  const [isClosedByUser, setIsClosedByUser] = useState(false);
  const [downloadProgressState, setDownloadProgressState] = useState({
    keys: [],
    currentIdx: 0,
    currentFileProgress: {},
  });

  useEffect(() => {
    if (modelsNeedDownload) {
      setIsClosedByUser(false);
    }
  }, [modelsNeedDownload]);

  const overallProgress = useMemo(() => {
    const { keys, currentIdx, currentFileProgress } = downloadProgressState;
    if (!keys || keys.length === 0) return 0;
    const progressValues = Object.values(currentFileProgress);
    const currentModelProgress = progressValues.length > 0
      ? progressValues.reduce((sum, val) => sum + val, 0) / Math.max(progressValues.length, 5)
      : 0;
    const progress = (currentIdx * 100 + currentModelProgress) / keys.length;
    return Math.max(0, Math.min(100, progress));
  }, [downloadProgressState]);

  useEffect(() => {
    window.dispatchEvent(
      new CustomEvent('model-download-progress', {
        detail: {
          active: modelDownloading && isClosedByUser,
          progress: Math.round(overallProgress),
        },
      }),
    );
  }, [modelDownloading, isClosedByUser, overallProgress]);

  useEffect(() => {
    if (modelDownloadLogRef?.current) {
      modelDownloadLogRef.current.scrollTop = modelDownloadLogRef.current.scrollHeight;
    }
  }, [modelDownloadLog]);

  useTauriEventListener('install-log', (event) => {
    const payload = event?.payload || {};
    const line = payload.line || JSON.stringify(payload);
    const ts = new Date().toLocaleTimeString();
    setModelDownloadLog((prev) => [...prev, `[${ts}] ${line}`]);

    if (payload.source === 'aria2' && payload.file) {
      const match = line.match(/\((\d+)%\)/);
      if (match) {
        const percent = parseInt(match[1], 10);
        setDownloadProgressState((prev) => {
          const newProgress = { ...prev.currentFileProgress, [payload.file]: percent };
          return {
            ...prev,
            currentFileProgress: newProgress,
          };
        });
      }
    }
  }, [modelDownloading], modelDownloading);

  useEffect(() => {
    if (!modelsNeedDownload || !missingModels) return;
    if (modelDownloadStartedRef.current || modelDownloading) return;
    if (modelDownloadError) return;
    if (renderVenvInstallStep != null || depsNeedUpdate) return;

    modelDownloadStartedRef.current = true;
    setModelDownloading(true);
    setModelDownloadLog([]);
    setModelDownloadError(null);

    (async () => {
      try {
        const keysToDownload = [];
        if (missingModels['chinese-clip'] && !missingModels['chinese-clip'].complete) {
          keysToDownload.push('chinese-clip');
        }
        if (missingModels['bge-small-zh'] && !missingModels['bge-small-zh'].complete) {
          keysToDownload.push('bge-small-zh');
        }
        if (missingModels['minilm-l12'] && !missingModels['minilm-l12'].complete) {
          keysToDownload.push('minilm-l12');
        }

        setDownloadProgressState({
          keys: keysToDownload,
          currentIdx: 0,
          currentFileProgress: {},
        });

        for (let i = 0; i < keysToDownload.length; i++) {
          const key = keysToDownload[i];
          setDownloadProgressState((prev) => ({
            ...prev,
            currentIdx: i,
            currentFileProgress: {},
          }));

          if (key === 'chinese-clip') {
            setModelDownloadLog((prev) => [...prev, `[${new Date().toLocaleTimeString()}] ${t('mask.model_download.downloading_clip')}`]);
            await invoke('download_model', { modelId: 'chinese-clip' });
          } else if (key === 'bge-small-zh') {
            setModelDownloadLog((prev) => [...prev, `[${new Date().toLocaleTimeString()}] ${t('mask.model_download.downloading_bge')}`]);
            await invoke('download_model', { modelId: 'bge-small-zh' });
          } else if (key === 'minilm-l12') {
            setModelDownloadLog((prev) => [...prev, `[${new Date().toLocaleTimeString()}] ${t('mask.model_download.downloading_minilm')}`]);
            await invoke('download_model', { modelId: 'minilm-l12' });
          }
        }

        setModelDownloadLog((prev) => [...prev, `[${new Date().toLocaleTimeString()}] ${t('mask.model_download.complete')}`]);
        onModelsDownloadComplete?.();
      } catch (err) {
        setModelDownloadError(err?.message || String(err));
      } finally {
        setModelDownloading(false);
        modelDownloadStartedRef.current = false;
      }
    })();
  }, [modelsNeedDownload, missingModels, modelDownloading, modelDownloadError, renderVenvInstallStep, depsNeedUpdate, onModelsDownloadComplete, retryNonce, t]);

  const retryModelDownload = () => {
    setModelDownloadError(null);
    modelDownloadStartedRef.current = false;
    setRetryNonce((value) => value + 1);
  };

  return {
    modelDownloadLog,
    modelDownloadError,
    modelDownloading,
    modelDownloadLogRef,
    isClosedByUser,
    setIsClosedByUser,
    retryModelDownload,
  };
}
