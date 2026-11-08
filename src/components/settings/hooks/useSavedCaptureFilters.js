import { useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getMonitorFilters, updateMonitorFilters } from '../../../lib/monitor_api';
import { filtersFromBackend, readSavedCaptureFilters } from './useCaptureFilterSettings';
import { setPreference } from '../../../lib/preference_store';

// Each authenticated UI host synchronizes the native committed rules. Local
// preferences migrate when no native file exists; editor drafts stay local.
export function useSavedCaptureFilters({ enabled = true }) {
  useEffect(() => {
    if (!enabled) return undefined;
    let disposed = false;
    let syncing = false;
    let again = false;
    let unlisten;
    const sync = async () => {
      if (disposed) return;
      if (syncing) { again = true; return; }
      syncing = true;
      try {
        do {
          again = false;
          const filters = await getMonitorFilters();
          if (disposed) return;
          if (filters) {
            setPreference('monitorFilters', JSON.stringify(filtersFromBackend(filters)));
          } else {
            const saved = readSavedCaptureFilters();
            await updateMonitorFilters({ processes: saved.processes, titles: saved.titles, ignore_protected: saved.ignoreProtected }, { promptMode: 'defer' });
          }
        } while (again && !disposed);
      } catch (error) {
        if (!disposed && !String(error).includes('AUTH_REQUIRED')) console.warn('Failed to synchronize capture filters:', error);
      } finally { syncing = false; }
    };
    void (async () => {
      try {
        const stop = await listen('capture-filters-changed', sync);
        if (disposed) { stop(); return; }
        unlisten = stop;
      } catch (error) {
        console.warn('Failed to listen for capture filter changes:', error);
      }
      await sync();
    })();
    return () => { disposed = true; unlisten?.(); };
  }, [enabled]);
}
