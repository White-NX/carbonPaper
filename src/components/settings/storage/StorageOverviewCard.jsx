import React from 'react';
import { useTranslation } from 'react-i18next';
import { FileDown, FileUp, HardDrive } from 'lucide-react';
import { formatBytes, formatTimestamp } from '../analysisUtils';
import { SettingsCard } from '../SettingsPrimitives';
import StorageRingChart from './StorageRingChart';

export default function StorageOverviewCard({
  storageSegments,
  totalStorage,
  storage,
  loading,
  diskInfo,
  onExportBackup,
  onImportBackup,
}) {
  const { t } = useTranslation();

  return (
    <SettingsCard padding="p-6">
      <div className="flex items-center gap-2 mb-6">
        <div className="p-2 rounded-lg bg-ide-bg border border-ide-border">
          <HardDrive className="w-4 h-4" />
        </div>
        <div>
          <h3 className="font-semibold">{t('settings.storageManagement.overview.title')}</h3>
          <p className="text-[11px] text-ide-muted">{storage?.root_path || t('settings.storageManagement.overview.path', { path: 'LocalAppData/CarbonPaper' })}</p>
        </div>
      </div>

      <div className="flex flex-col lg:flex-row items-center gap-8">
        <div className="flex-shrink-0">
          <StorageRingChart
            totalDiskUsed={diskInfo.usedSize}
            totalDiskSize={diskInfo.totalSize}
            appUsedBytes={totalStorage}
            loading={loading}
          />
        </div>

        <div className="flex-1 space-y-4">
          <div className="space-y-2">
            <div className="flex items-center gap-3">
              <div className="w-3 h-3 rounded-full bg-gradient-to-r from-purple-500 to-purple-400" />
              <span className="text-sm text-ide-muted">{t('settings.storageManagement.overview.disk_used')}</span>
              <span className="text-sm font-medium ml-auto">{formatBytes(diskInfo.usedSize)}</span>
            </div>
            <div className="flex items-center gap-3">
              <div className="w-3 h-3 rounded-full bg-gradient-to-r from-blue-500 to-blue-400" />
              <span className="text-sm text-ide-muted">{t('settings.storageManagement.overview.program_used')}</span>
              <span className="text-sm font-medium ml-auto">{formatBytes(totalStorage)}</span>
            </div>
          </div>

          <div className="grid grid-cols-2 gap-2 pt-2 border-t border-ide-border/50">
            {storageSegments.map((segment) => {
              const Icon = segment.icon;
              return (
                <div key={segment.key} className="flex items-center gap-2 text-xs">
                  <Icon className="w-3.5 h-3.5 text-ide-muted" />
                  <span className="text-ide-muted">{segment.label}</span>
                  <span className="ml-auto font-medium">{formatBytes(segment.bytes)}</span>
                </div>
              );
            })}
          </div>

          <div className="text-[11px] text-ide-muted pt-2">
            {t('settings.storageManagement.last_updated', { time: storage?.cached_at_ms ? formatTimestamp(storage.cached_at_ms) : '--' })}
          </div>

          <div className="flex gap-2 pt-2">
            <button
              type="button"
              onClick={onExportBackup}
              className="flex-1 flex items-center justify-center gap-2 px-3 py-2 text-xs border border-ide-border rounded-lg bg-ide-bg hover:border-ide-accent hover:text-ide-accent transition-colors"
            >
              <FileDown className="w-3.5 h-3.5" />
              {t('settings.storageManagement.backup.export', 'Export Backup')}
            </button>
            <button
              type="button"
              onClick={onImportBackup}
              className="flex-1 flex items-center justify-center gap-2 px-3 py-2 text-xs border border-ide-border rounded-lg bg-ide-bg hover:border-ide-accent hover:text-ide-accent transition-colors"
            >
              <FileUp className="w-3.5 h-3.5" />
              {t('settings.storageManagement.backup.import', 'Import Backup')}
            </button>
          </div>
        </div>
      </div>
    </SettingsCard>
  );
}
