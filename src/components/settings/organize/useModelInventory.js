import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../../lib/auth_api';

export function useModelInventory() {
  const [models, setModels] = useState([]);
  const [modelsLoading, setModelsLoading] = useState(false);

  const loadModels = async () => {
    setModelsLoading(true);
    try {
      const res = await withAuth(() => invoke('get_model_inventory'));
      const parsedRes = typeof res === 'string' ? JSON.parse(res) : res;
      if (parsedRes && parsedRes.status === 'success' && Array.isArray(parsedRes.models)) {
        setModels(parsedRes.models);
      } else {
        console.warn('[ModelInventory] Response format unexpected or not successful:', parsedRes);
      }
    } catch (err) {
      console.error('[ModelInventory] Failed to fetch models:', err);
    } finally {
      setModelsLoading(false);
    }
  };

  const handleOpenLocation = async (path) => {
    try {
      await withAuth(() => invoke('open_path', { path }), { autoPrompt: true });
    } catch (err) {
      console.error('Failed to open location:', err);
    }
  };

  const formatSize = (bytes) => {
    if (!Number.isFinite(bytes) || bytes <= 0) return '-';
    const units = ['B', 'KB', 'MB', 'GB'];
    let value = bytes;
    let unit = 0;
    while (value >= 1024 && unit < units.length - 1) {
      value /= 1024;
      unit += 1;
    }
    return `${unit === 0 ? value : value.toFixed(1)} ${units[unit]}`;
  };

  useEffect(() => {
    loadModels();
  }, []);

  return {
    models,
    modelsLoading,
    loadModels,
    handleOpenLocation,
    formatSize,
  };
}
