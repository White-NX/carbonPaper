import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

export function useRequiredModels() {
  const [modelsCheckDone, setModelsCheckDone] = useState(false);
  const [modelsNeedDownload, setModelsNeedDownload] = useState(false);
  const [missingModels, setMissingModels] = useState(null);

  const refreshRequiredModels = useCallback(async () => {
    try {
      const modelStatus = await invoke('check_model_files');
      const hasIncomplete = Object.values(modelStatus).some((m) => !m.complete && m.required !== false);
      if (hasIncomplete) {
        setModelsNeedDownload(true);
        setMissingModels(modelStatus);
      } else {
        setModelsNeedDownload(false);
        setMissingModels(null);
      }
    } catch (err) {
      console.warn('Failed to check model files:', err);
      setModelsNeedDownload(false);
    } finally {
      setModelsCheckDone(true);
    }
  }, []);

  useEffect(() => {
    refreshRequiredModels();
  }, [refreshRequiredModels]);

  const handleModelsDownloadComplete = useCallback(() => {
    setModelsNeedDownload(false);
    setMissingModels(null);
  }, []);

  return {
    modelsCheckDone,
    modelsNeedDownload,
    missingModels,
    refreshRequiredModels,
    handleModelsDownloadComplete,
  };
}
