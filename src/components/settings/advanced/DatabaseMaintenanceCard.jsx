import React from 'react';
import { useTranslation } from 'react-i18next';
import { Database, Loader2 } from 'lucide-react';

export default function DatabaseMaintenanceCard({
  vacuumRunning,
  vacuumMessage,
  onManualVacuum,
}) {
  const { t } = useTranslation();

  return (
    <div className="space-y-3">
      <label className="text-sm font-semibold text-ide-accent px-1 flex items-center gap-2">
        <Database className="w-4 h-4" />
        {t('settings.advanced.vacuum.title', '数据库维护')}
      </label>

      <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-4">
        <div className="flex items-center justify-between gap-4">
          <div className="flex-1 min-w-0">
            <p className="text-sm text-ide-text font-medium">{t('settings.advanced.vacuum.label', '手动执行数据库优化')}</p>
            <p className="text-xs text-ide-muted mt-1">{t('settings.advanced.vacuum.description', '执行 VACUUM 可回收数据库空间并整理存储结构，过程可能持续数秒到数分钟。')}</p>
          </div>
          <button
            onClick={onManualVacuum}
            disabled={vacuumRunning}
            className="shrink-0 flex items-center gap-2 px-3 py-2 rounded-lg text-xs font-medium transition-colors border border-ide-border bg-ide-panel hover:bg-ide-hover text-ide-text disabled:opacity-60"
          >
            {vacuumRunning && <Loader2 className="w-3.5 h-3.5 animate-spin" />}
            {vacuumRunning
              ? t('settings.advanced.vacuum.running', '优化中...')
              : t('settings.advanced.vacuum.action', '立即优化')}
          </button>
        </div>

        {vacuumMessage && (
          <div className="text-xs text-ide-muted bg-ide-panel/50 border border-ide-border/30 rounded-lg px-3 py-2">
            {vacuumMessage}
          </div>
        )}
      </div>
    </div>
  );
}
