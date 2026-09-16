import { useEffect } from 'react';
import { updateMonitorFilters } from '../../../lib/monitor_api';
import { readSavedCaptureFilters } from './useCaptureFilterSettings';

// This effect belongs to the app lifecycle, not an open settings page. A
// restart always reads committed preferences; an editor's draft stays local.
export function useSavedCaptureFilters({ monitorStatus, enabled = true }) {
  useEffect(() => {
    if (!enabled || monitorStatus !== 'running') return;
    const saved = readSavedCaptureFilters();
    updateMonitorFilters({ processes: saved.processes, titles: saved.titles, ignore_protected: saved.ignoreProtected })
      .catch((error) => console.warn('Failed to apply saved capture filters:', error));
  }, [monitorStatus, enabled]);
}
