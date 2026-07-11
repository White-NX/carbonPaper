import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../../lib/auth_api';

export function useModelInventory({ monitorStatus }) {
  const [models, setModels] = useState([]);
  const [modelsLoading, setModelsLoading] = useState(false);

  const loadModels = async () => {
    if (monitorStatus !== 'running') {
      return;
    }
    setModelsLoading(true);
    try {
      const res = await withAuth(() => invoke('monitor_get_all_models'));
      const parsedRes = typeof res === 'string' ? JSON.parse(res) : res;
      if (parsedRes && parsedRes.status === 'success' && parsedRes.models) {
        setModels(parsedRes.models);
      } else {
        console.warn('[FeaturesSection] Response format unexpected or not successful:', parsedRes);
      }
    } catch (err) {
      console.error('[FeaturesSection] Failed to fetch models:', err);
    } finally {
      setModelsLoading(false);
    }
  };

  const handleOpenLocation = async (path) => {
    try {
      await invoke('open_path', { path });
    } catch (err) {
      console.error('Failed to open location:', err);
    }
  };

  const formatSize = (sizeStr) => {
    if (!sizeStr) return '-';
    return sizeStr;
  };

  useEffect(() => {
    if (monitorStatus === 'running') {
      loadModels();
    }
  }, [monitorStatus]);

  return {
    models,
    modelsLoading,
    loadModels,
    handleOpenLocation,
    formatSize,
  };
}
