import { useCallback, useEffect, useState } from 'react';
import { getAnalysisOverview } from '../../../lib/analysis_api';
import { REFRESH_INTERVAL_MS } from '../analysisUtils';

export function useStorageAnalysisOverview({ isOpen, activeTab, t }) {
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

  useEffect(() => {
    if (!isOpen || activeTab !== 'maintenance') return undefined;
    loadAnalysisOverview(false);
    const timer = setInterval(() => loadAnalysisOverview(false), REFRESH_INTERVAL_MS);
    return () => clearInterval(timer);
  }, [isOpen, activeTab, loadAnalysisOverview]);

  return {
    storage,
    analysisLoading,
    analysisRefreshing,
    analysisError,
    handleRefreshAnalysis,
  };
}
