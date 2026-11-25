import React from 'react';
import useUsageStatistics from '../../hooks/useUsageStatistics';
import UsageStatisticsView from './UsageStatisticsView';

export default function UsageStatistics({ active, isAuthenticated, onViewSnapshots, onOpenRecap }) {
  const statistics = useUsageStatistics(active, isAuthenticated);
  if (!active || !isAuthenticated) return null;
  return <UsageStatisticsView {...statistics} onViewSnapshots={onViewSnapshots} onOpenRecap={onOpenRecap} />;
}
