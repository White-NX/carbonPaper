import { useAutoLaunchStatus } from './hooks/useAutoLaunchStatus';
import { useCaptureFilterSettings } from './hooks/useCaptureFilterSettings';
import { useGeneralPreferenceFlags } from './hooks/useGeneralPreferenceFlags';
import { useMonitorControls } from './hooks/useMonitorControls';
import { useStorageAnalysisOverview } from './hooks/useStorageAnalysisOverview';
import { useUpdateCheck } from './hooks/useUpdateCheck';

export function useSettingsDialogController({
  isOpen,
  activeTab,
  onManualStartMonitor,
  onManualStopMonitor,
  onRecordsDeleted,
  t,
}) {
  const generalPreferences = useGeneralPreferenceFlags();
  const monitorControls = useMonitorControls({
    isOpen,
    onManualStartMonitor,
    onManualStopMonitor,
  });
  const captureFilters = useCaptureFilterSettings({
    monitorStatus: monitorControls.monitorStatus,
    onRecordsDeleted,
    t,
  });
  const autoLaunchStatus = useAutoLaunchStatus({ isOpen, t });
  const storageAnalysis = useStorageAnalysisOverview({ isOpen, activeTab, t });
  const updateCheck = useUpdateCheck();

  return {
    ...generalPreferences,
    ...monitorControls,
    ...captureFilters,
    ...autoLaunchStatus,
    ...storageAnalysis,
    ...updateCheck,
  };
}
