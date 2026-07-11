import React from 'react';
import { useTranslation } from 'react-i18next';
import { ChevronRight, PieChart } from 'lucide-react';
import { SettingsCard } from '../SettingsPrimitives';
import ProcessDistributionProgress from './ProcessDistributionProgress';
import { PROCESS_PALETTE } from './storageConstants';

export default function ProcessStorageCard({
  deleteQueueStatus,
  processStats,
  processStatsLoading,
  processStatsError,
  onOpenProcessDetail,
}) {
  const { t } = useTranslation();

  return (
    <SettingsCard>
      <div className="flex items-center justify-between gap-3">
        <div className="flex items-center gap-3">
          <div className="p-2 rounded-lg bg-ide-bg border border-ide-border">
            <PieChart className="w-4 h-4" />
          </div>
          <div>
            <h3 className="font-semibold">{t('settings.storageManagement.processDetails.title')}</h3>
            <p className="text-xs text-ide-muted">{t('settings.storageManagement.processDetails.description')}</p>
          </div>
        </div>
      </div>

      {deleteQueueStatus.running && (
        <div className="mt-4 text-xs text-ide-muted">
          {t('settings.storageManagement.processDetails.queueRunning', {
            ocr: deleteQueueStatus.pending_ocr || 0,
            screenshots: deleteQueueStatus.pending_screenshots || 0,
          })}
        </div>
      )}

      {processStatsError && (
        <div className="mt-4 text-xs text-red-400">{processStatsError}</div>
      )}

      <div className="mt-4 border border-ide-border rounded-xl p-4 bg-ide-bg/50">
        <ProcessDistributionProgress stats={processStats} loading={processStatsLoading} />
      </div>

      <div className="mt-4 space-y-2 overflow-y-auto pr-1">
        {(processStats || []).map((item, idx) => {
          const key = item.process_name || `unknown-${idx}`;
          const percent = Number(item.percentage || 0).toFixed(2);
          const hasProcessName = Boolean(item.process_name);
          return (
            <button
              key={key}
              type="button"
              disabled={!hasProcessName}
              onClick={() => onOpenProcessDetail(item.process_name)}
              className="w-full text-left border border-ide-border rounded-xl p-3 bg-ide-bg/70 transition-colors hover:border-ide-accent/70 focus:outline-none focus:ring-1 focus:ring-ide-accent/40 disabled:opacity-60 disabled:cursor-not-allowed"
            >
              <div className="flex items-center justify-between gap-2">
                <div className="min-w-0">
                  <div className="text-sm font-medium truncate">{item.process_name || t('settings.storageManagement.processDetails.unknownProcess')}</div>
                  <div className="text-xs text-ide-muted">{t('settings.storageManagement.processDetails.itemSummary', { count: item.screenshot_count || 0, percent })}</div>
                </div>
                {hasProcessName && <ChevronRight className="w-4 h-4 text-ide-muted shrink-0" />}
              </div>
              <div className="mt-2 h-1.5 bg-ide-panel rounded-full overflow-hidden">
                <div
                  className="h-full"
                  style={{
                    width: `${Math.max(2, Number(item.percentage || 0))}%`,
                    backgroundColor: PROCESS_PALETTE[idx % PROCESS_PALETTE.length],
                  }}
                />
              </div>
            </button>
          );
        })}

        {!processStatsLoading && (!processStats || processStats.length === 0) && (
          <div className="text-xs text-ide-muted">{t('settings.storageManagement.processDetails.noStats')}</div>
        )}
      </div>
    </SettingsCard>
  );
}
