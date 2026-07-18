import React from 'react';
import { useTranslation } from 'react-i18next';
import { RefreshCw } from 'lucide-react';
import { PROCESS_PALETTE } from './storageConstants';

export default function ProcessDistributionProgress({ stats, loading }) {
  const { t } = useTranslation();
  const total = (stats || []).reduce((sum, item) => sum + (item.screenshot_count || 0), 0);
  const topStats = (stats || []).slice(0, 8).map((item) => ({
    ...item,
    percent: total > 0 ? ((item.screenshot_count || 0) / total) * 100 : 0,
  }));
  const othersCount = (stats || []).slice(8).reduce((sum, item) => sum + (item.screenshot_count || 0), 0);
  const segments = othersCount > 0
    ? [...topStats, { process_name: t('settings.storageManagement.processDetails.others'), screenshot_count: othersCount, percent: total > 0 ? (othersCount / total) * 100 : 0 }]
    : topStats;

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between text-xs text-ide-muted">
        <span>{t('settings.storageManagement.processDetails.distributionTitle')}</span>
        {!loading && <span>{t('settings.storageManagement.processDetails.totalScreenshots', { count: total })}</span>}
      </div>

      {loading && (
        <div className="py-4 flex items-center justify-center">
          <RefreshCw className="w-5 h-5 animate-spin text-ide-muted" />
        </div>
      )}

      {!loading && topStats.length === 0 && (
        <div className="text-xs text-ide-muted py-2">{t('settings.storageManagement.processDetails.noStats')}</div>
      )}

      {!loading && topStats.length > 0 && (
        <div className="space-y-3">
          <div className="h-5 rounded-full overflow-hidden bg-ide-bg/70 flex">
            {segments.map((item, idx) => {
              const percent = Number(item.percent || 0);
              if (percent <= 0) return null;
              return (
                <div
                  key={`${item.process_name || 'unknown'}-${idx}`}
                  className="h-full transition-all duration-500"
                  style={{
                    width: `${Math.max(1, percent)}%`,
                    backgroundColor: PROCESS_PALETTE[idx % PROCESS_PALETTE.length],
                  }}
                  title={`${item.process_name || t('settings.storageManagement.processDetails.unknownProcess')} ${percent.toFixed(2)}%`}
                />
              );
            })}
          </div>

          <div className="grid grid-cols-1 sm:grid-cols-2 gap-2">
            {segments.map((item, idx) => {
              const percent = Number(item.percent || 0);
              return (
                <div key={`${item.process_name || 'unknown'}-legend-${idx}`} className="flex items-center justify-between gap-2 text-xs">
                  <div className="flex items-center gap-2 min-w-0">
                    <span
                      className="w-2.5 h-2.5 rounded-full shrink-0"
                      style={{ backgroundColor: PROCESS_PALETTE[idx % PROCESS_PALETTE.length] }}
                    />
                    <span className="truncate">{item.process_name || t('settings.storageManagement.processDetails.unknownProcess')}</span>
                  </div>
                  <span className="text-ide-muted shrink-0">{t('settings.storageManagement.processDetails.itemSummary', { count: item.screenshot_count || 0, percent: percent.toFixed(2) })}</span>
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
