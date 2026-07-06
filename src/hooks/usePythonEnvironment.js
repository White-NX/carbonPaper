import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

export function usePythonEnvironment() {
  const [pythonVersion, setPythonVersion] = useState(null);
  const [depsNeedUpdate, setDepsNeedUpdate] = useState(false);
  const [depsSyncing, setDepsSyncing] = useState(false);
  const [depsCheckDone, setDepsCheckDone] = useState(false);
  const [modelsNeedDownload, setModelsNeedDownload] = useState(false);
  const [missingModels, setMissingModels] = useState(null);

  const refreshPythonVersion = useCallback(async () => {
    try {
      const version = await invoke('check_python_venv');
      setPythonVersion(version);

      if (version) {
        try {
          const result = await invoke('check_deps_freshness');
          if (result?.needs_update) {
            setDepsNeedUpdate(true);
          } else {
            setDepsNeedUpdate(false);
          }
        } catch (err) {
          console.warn('Failed to check deps freshness:', err);
          setDepsNeedUpdate(false);
        }

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
        }
      }
    } catch (error) {
      console.error('Error fetching Python version:', error);
    } finally {
      setDepsCheckDone(true);
    }
  }, []);

  useEffect(() => {
    refreshPythonVersion();
  }, [refreshPythonVersion]);

  const handleDepsSync = useCallback(async () => {
    setDepsSyncing(true);
    try {
      await invoke('sync_python_deps');
      setDepsNeedUpdate(false);
    } catch (err) {
      throw err;
    } finally {
      setDepsSyncing(false);
    }
  }, []);

  const handleModelsDownloadComplete = useCallback(() => {
    setModelsNeedDownload(false);
    setMissingModels(null);
  }, []);

  return {
    pythonVersion,
    depsNeedUpdate,
    depsSyncing,
    depsCheckDone,
    modelsNeedDownload,
    missingModels,
    refreshPythonVersion,
    handleDepsSync,
    handleModelsDownloadComplete,
  };
}
