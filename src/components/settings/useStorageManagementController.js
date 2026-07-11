import { useCallback, useMemo } from 'react';
import { useIndexHealthStatus } from './storage/useIndexHealthStatus';
import { useProcessStorageDetails } from './storage/useProcessStorageDetails';
import { useStorageMigration } from './storage/useStorageMigration';
import { useStoragePolicy } from './storage/useStoragePolicy';

export function useStorageManagementController({ storage, onRefresh, t, monitorStatus }) {
  const storagePolicy = useStoragePolicy({ t });
  const storageMigration = useStorageMigration({ storage, onRefresh, t });
  const processDetails = useProcessStorageDetails({ onRefresh, t });
  const indexHealthStatus = useIndexHealthStatus({ monitorStatus, t });

  const diskInfo = useMemo(() => {
    const rootPath = storage?.root_path || '';
    const driveLetter = rootPath.charAt(0);

    return {
      driveLetter: driveLetter || 'C',
      totalSize: 500 * 1024 * 1024 * 1024,
      usedSize: 320 * 1024 * 1024 * 1024,
    };
  }, [storage]);

  const handleRefresh = useCallback(() => {
    onRefresh?.();
    processDetails.loadDeleteQueueStatus();
    indexHealthStatus.loadIndexHealth({ refreshVector: monitorStatus === 'running' });
    if (processDetails.panelView === 'overview') {
      processDetails.loadProcessStats();
    }
    if (processDetails.panelView === 'process-detail' && processDetails.selectedProcess) {
      processDetails.loadProcessMonthPage(processDetails.selectedProcess, processDetails.processPage);
    }
  }, [
    indexHealthStatus,
    monitorStatus,
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
