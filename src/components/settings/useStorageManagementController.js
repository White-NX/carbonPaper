import { useCallback, useMemo } from 'react';
import { useIndexHealthStatus } from './storage/useIndexHealthStatus';
import { useProcessStorageDetails } from './storage/useProcessStorageDetails';
import { useStorageMigration } from './storage/useStorageMigration';
import { useStoragePolicy } from './storage/useStoragePolicy';

export function useStorageManagementController({ storage, onRefresh, t }) {
  const storagePolicy = useStoragePolicy({ t });
  const storageMigration = useStorageMigration({ storage, onRefresh, t });
  const processDetails = useProcessStorageDetails({ onRefresh, t });
  const indexHealthStatus = useIndexHealthStatus();

  const diskInfo = useMemo(() => {
    const rootPath = storage?.root_path || '';
    const driveLetter = rootPath.charAt(0);

    // The backend reports the two raw facts — capacity and free space — and used
    // space is their difference. Either one missing means the hosting volume
    // could not be resolved, so both stay null and the UI reads "unknown"
    // instead of drawing a zero as an empty disk.
    const totalSize = storage?.disk_total_bytes ?? null;
    const availableSize = storage?.disk_available_bytes ?? null;
    const usedSize =
      totalSize !== null && availableSize !== null
        ? Math.max(totalSize - availableSize, 0)
        : null;

    return {
      driveLetter: driveLetter || 'C',
      totalSize,
      usedSize,
    };
  }, [storage]);

  const handleRefresh = useCallback(() => {
    onRefresh?.();
    processDetails.loadDeleteQueueStatus();
    indexHealthStatus.loadIndexHealth();
    if (processDetails.panelView === 'overview') {
      processDetails.loadProcessStats();
    }
    if (processDetails.panelView === 'process-detail' && processDetails.selectedProcess) {
      processDetails.loadProcessMonthPage(processDetails.selectedProcess, processDetails.processPage);
    }
  }, [
    indexHealthStatus,
    onRefresh,
    processDetails,
  ]);

  return {
    ...storagePolicy,
    ...storageMigration,
    ...processDetails,
    ...indexHealthStatus,
    diskInfo,
    handleRefresh,
  };
}
