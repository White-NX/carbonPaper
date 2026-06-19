import { invoke } from '@tauri-apps/api/core';
import { withAuth } from './auth_api';

export const getAnalysisOverview = async (forceStorage = false) => {
  try {
    return await withAuth(() => invoke('get_analysis_overview', { forceStorage }));
  } catch (error) {
    console.error('Failed to get analysis overview', error);
    throw error;
  }
};
