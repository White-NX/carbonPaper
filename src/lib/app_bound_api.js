import { invoke } from '@tauri-apps/api/core';
import { withAuth } from './auth_api';

export const getAppBoundStatus = () => invoke('app_bound_status');
export const dismissAppBoundOffer = () => invoke('app_bound_acknowledge_offer');
export const installAppBound = (enable = true) => withAuth(
  () => invoke('app_bound_install', { enable }), { autoPrompt: true },
);
export const repairAppBound = ({ packagePath = null, allowFullDownload = false } = {}) => withAuth(
  () => invoke('app_bound_repair', { packagePath, allowFullDownload }), { autoPrompt: true },
);
export const setAppBoundPolicy = (enabled, retentionDays, capacityMiB) => withAuth(
  () => invoke('app_bound_set_policy', { enabled, retentionDays, capacityMib: capacityMiB }),
  { autoPrompt: true },
);
export const uninstallAppBound = () => withAuth(
  () => invoke('app_bound_uninstall'), { autoPrompt: true },
);

export function appBoundErrorMessage(error, t) {
  const value = String(error);
  if (value.includes('CANCELLED') || value.includes('AUTH_CANCELLED')) {
    return t('appBound.cancelled');
  }
  if (value.includes('BACKGROUND_PROCESSING_DISABLED')) {
    return t('appBound.backgroundDisabled');
  }
  const repairErrors = [
    ['REPAIR_NETWORK_DISABLED', 'repairNetworkDisabled'],
    ['REPAIR_DOWNLOAD_FAILED', 'repairDownloadFailed'],
    ['REPAIR_NOT_FOUND', 'repairDownloadFailed'],
    ['REPAIR_PACKAGE_INVALID', 'repairPackageInvalid'],
    ['REPAIR_VERSION_MISMATCH', 'repairVersionMismatch'],
    ['REPAIR_FULL_RUNTIME_REQUIRED', 'repairFullRequired'],
    ['REPAIR_SERVICE_UNAVAILABLE', 'repairServiceUnavailable'],
    ['REPAIR_BUSY', 'repairBusy'],
  ];
  for (const [code, key] of repairErrors) {
    if (value.includes(code)) return t(`appBound.${key}`);
  }
  if (value.includes('PACKAGE_UNAVAILABLE')) {
    return t('appBound.packageUnavailable');
  }
  return t('appBound.failed');
}
