import React from 'react';
import { useTranslation } from 'react-i18next';
import { ArrowLeft, ChevronLeft, ChevronRight, RefreshCw, Trash2 } from 'lucide-react';
import { ThumbnailCard } from '../../ThumbnailCard';
import { SettingsCard } from '../SettingsPrimitives';

export default function ProcessDetailView({
  selectedProcess,
  processPage,
  processMonthData,
  processMonthLoading,
  processMonthError,
  processThumbMap,
  selectedScreenshotIds,
  deletingTarget,
  groupedMonthItems,
  selectedCountByMonth,
  onBack,
  onRequestSoftDelete,
  onToggleScreenshotSelection,
  onLoadProcessMonthPage,
}) {
  const { t } = useTranslation();

  return (
    <SettingsCard className="space-y-4">
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={onBack}
            className="inline-flex items-center gap-2 px-3 py-2 text-xs border border-ide-border rounded-lg bg-ide-panel hover:border-ide-accent hover:text-ide-accent transition-colors"
          >
            <ArrowLeft className="w-3.5 h-3.5" /> {t('settings.storageManagement.processDetails.back')}
          </button>
          <div>
            <div className="font-semibold text-sm">{selectedProcess || t('settings.storageManagement.processDetails.unknownProcess')}</div>
            <div className="text-xs text-ide-muted">{t('settings.storageManagement.processDetails.detailSubtitle')}</div>
          </div>
        </div>

        <button
          type="button"
          onClick={() => onRequestSoftDelete(selectedProcess, null)}
          disabled={deletingTarget === `${selectedProcess}::all`}
          className="inline-flex items-center gap-1 px-2 py-1 text-xs rounded-lg border border-red-500/40 text-red-300 hover:bg-red-500/10 disabled:opacity-60"
        >
          <Trash2 className="w-3.5 h-3.5" />
          {deletingTarget === `${selectedProcess}::all` ? t('settings.storageManagement.processDetails.deleting') : t('settings.storageManagement.processDetails.deleteProcess')}
        </button>
      </div>

      {processMonthError && <div className="text-xs text-red-400">{processMonthError}</div>}

      {processMonthLoading && (
        <div className="text-xs text-ide-muted inline-flex items-center gap-2">
          <RefreshCw className="w-3.5 h-3.5 animate-spin" /> {t('settings.storageManagement.processDetails.loading')}
        </div>
      )}

      {!processMonthLoading && groupedMonthItems.length === 0 && (
        <div className="text-xs text-ide-muted">{t('settings.storageManagement.processDetails.empty')}</div>
      )}

      {groupedMonthItems.map(([month, items]) => {
        const monthDeleteKey = `${selectedProcess}::${month}`;
        const deletingMonth = deletingTarget === monthDeleteKey;
        const monthDeletable = /^\d{4}-\d{2}$/.test(month);
        const selectedInMonth = items
          .map((item) => item.screenshot_id)
          .filter((id) => selectedScreenshotIds.has(id));
        const selectedCount = selectedCountByMonth[month] || 0;
        return (
          <div key={month} className="space-y-2">
            <div className="flex items-center justify-between">
              <div className="text-sm font-medium">{month}</div>
              <button
                type="button"
                onClick={() => onRequestSoftDelete(selectedProcess, month, selectedInMonth)}
                disabled={deletingMonth || (selectedCount === 0 && !monthDeletable)}
                className="inline-flex items-center gap-1 px-2 py-1 text-xs rounded-lg border border-red-500/40 text-red-300 hover:bg-red-500/10 disabled:opacity-60"
              >
                <Trash2 className="w-3.5 h-3.5" />
                {deletingMonth
                  ? t('settings.storageManagement.processDetails.deleting')
                  : selectedCount > 0
                    ? t('settings.storageManagement.processDetails.deleteSelected', { count: selectedCount })
                    : t('settings.storageManagement.processDetails.deleteMonth')}
              </button>
            </div>

            <div className="grid grid-cols-3 gap-2">
              {items.map((item) => {
                const selected = selectedScreenshotIds.has(item.screenshot_id);
                const thumbSrc = processThumbMap?.[String(item.screenshot_id)] || null;
                return (
                  <div
                    key={item.screenshot_id}
                    className={`relative rounded ${selected ? 'ring-2 ring-ide-accent/80' : ''}`}
                    title={item.created_at}
                  >
                    <ThumbnailCard
                      item={{
                        screenshot_id: item.screenshot_id,
                        image_path: item.image_path,
                        process_name: selectedProcess,
                        window_title: item.created_at,
                        created_at: item.created_at,
                      }}
                      preloadedSrc={thumbSrc}
                      footerText={item.created_at}
                      footerPersistent
                      onSelect={(payload) => {
                        const id = payload?.screenshot_id ?? payload?.id;
                        onToggleScreenshotSelection(id);
                      }}
                    />
                    {selected && (
                      <div className="pointer-events-none absolute top-1.5 left-1.5 px-1.5 py-0.5 rounded text-[10px] font-medium bg-ide-accent text-white">
                        {t('settings.storageManagement.processDetails.selected')}
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          </div>
        );
      })}

      <div className="flex items-center justify-end gap-2">
        <button
          type="button"
          onClick={() => onLoadProcessMonthPage(selectedProcess, Math.max(0, processPage - 1))}
          disabled={processPage <= 0 || processMonthLoading}
          className="inline-flex items-center gap-1 px-2 py-1 text-xs rounded-lg border border-ide-border bg-ide-panel disabled:opacity-60"
        >
          <ChevronLeft className="w-3.5 h-3.5" /> {t('settings.storageManagement.processDetails.prevPage')}
        </button>
        <div className="text-xs text-ide-muted">{t('settings.storageManagement.processDetails.page', { page: processPage + 1 })}</div>
        <button
          type="button"
          onClick={() => {
            if (processMonthData?.next_page !== null && processMonthData?.next_page !== undefined) {
              onLoadProcessMonthPage(selectedProcess, processMonthData.next_page);
            }
          }}
          disabled={processMonthData?.next_page === null || processMonthData?.next_page === undefined || processMonthLoading}
          className="inline-flex items-center gap-1 px-2 py-1 text-xs rounded-lg border border-ide-border bg-ide-panel disabled:opacity-60"
        >
          {t('settings.storageManagement.processDetails.nextPage')} <ChevronRight className="w-3.5 h-3.5" />
        </button>
      </div>
    </SettingsCard>
  );
}
