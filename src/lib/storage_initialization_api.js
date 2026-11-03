import { invoke } from '@tauri-apps/api/core';

export const getStorageInitializationStatus = () => invoke('storage_get_initialization_status');
export const retryStorageInitialization = () => invoke('storage_retry_initialization');
