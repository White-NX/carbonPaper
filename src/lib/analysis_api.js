import { invoke } from '@tauri-apps/api/core';

export const getAnalysisOverview = async (forceStorage = false) => {
  try {
    return await invoke('get_analysis_overview', { forceStorage });
  } catch (error) {
    console.error('Failed to get analysis overview', error);
    throw error;
  }
};
