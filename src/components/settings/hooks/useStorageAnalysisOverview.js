import { useCallback, useState } from 'react';
import { getAnalysisOverview } from '../../../lib/analysis_api';
import { REFRESH_INTERVAL_MS } from '../analysisUtils';
import { usePolling } from '../../../hooks/usePolling';
import { useSettingsWindowActivity } from '../SettingsActivityContext';

export function useStorageAnalysisOverview({ isOpen, activeTab, t }) {
  const { busy } = useSettingsWindowActivity();
  const [storage, setStorage] = useState(null);
  const [analysisLoading, setAnalysisLoading] = useState(true);
  const [analysisRefreshing, setAnalysisRefreshing] = useState(false);
  const [analysisError, setAnalysisError] = useState('');

  const loadAnalysisOverview = useCallback(
    async (forceStorage = false) => {
      try {
        setAnalysisError('');
        if (!analysisRefreshing) {
          setAnalysisLoading(true);
        }
        const result = await getAnalysisOverview(forceStorage);
        setStorage(result?.storage || null);
      } catch (err) {
        setAnalysisError(err?.message || t('settings.analysis.load_failed', { error: '' }));
      } finally {
        setAnalysisLoading(false);
        setAnalysisRefreshing(false);
      }
    },
    [analysisRefreshing, t],
  );

  const handleRefreshAnalysis = () => {
    setAnalysisRefreshing(true);
    loadAnalysisOverview(true);
  };

  usePolling(() => loadAnalysisOverview(false), {
    intervalMs: REFRESH_INTERVAL_MS,
    enabled: isOpen && activeTab === 'maintenance' && !busy,
  });

  return {
    storage,
    analysisLoading,
    analysisRefreshing,
    analysisError,
    handleRefreshAnalysis,
  };
}
