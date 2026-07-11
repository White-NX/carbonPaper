import React from 'react';
import { useTranslation } from 'react-i18next';
import { Database, ExternalLink, RefreshCw } from 'lucide-react';

export default function ModelInventoryTable({
  models,
  modelsLoading,
  monitorStatus,
  onRefresh,
  onOpenLocation,
  formatSize,
}) {
  const { t } = useTranslation();

  return (
    <section className="space-y-3 mt-8">
      <div className="flex items-center justify-between px-1">
        <label className="text-sm font-semibold text-ide-accent flex items-center gap-2">
          <Database className="w-4 h-4" />
          {t('settings.features.models.title', '模型管理')}
        </label>
        <button
          onClick={onRefresh}
          disabled={modelsLoading || monitorStatus !== 'running'}
          className="p-1 text-ide-muted hover:text-ide-text hover:bg-ide-hover rounded transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          title={t('settings.features.models.refresh', '刷新')}
        >
          <RefreshCw className={`w-4 h-4 ${modelsLoading ? 'animate-spin' : ''}`} />
        </button>
      </div>

      <div className="bg-ide-bg border border-ide-border rounded-xl overflow-hidden">
        <table className="w-full text-xs">
          <thead className="bg-ide-panel">
            <tr>
              <th className="px-3 py-2 text-left font-medium text-ide-text whitespace-nowrap">{t('settings.features.models.name', '模型')}</th>
              <th className="px-3 py-2 text-left font-medium text-ide-text whitespace-nowrap">{t('settings.features.models.purpose', '用途')}</th>
              <th className="px-3 py-2 text-left font-medium text-ide-text whitespace-nowrap">{t('settings.features.models.size', '大小')}</th>
              <th className="px-3 py-2 text-left font-medium text-ide-text">{t('settings.features.models.location', '位置')}</th>
            </tr>
          </thead>
          <tbody>
            {models.length > 0 ? (
              models.map((model) => (
                <tr key={model.name} className="border-t border-ide-border hover:bg-ide-hover transition-colors">
                  <td className="px-3 py-1.5 font-medium">{model.name}</td>
                  <td className="px-3 py-1.5 text-ide-muted whitespace-nowrap">{model.purpose}</td>
                  <td className="px-3 py-1.5 text-ide-muted whitespace-nowrap">{formatSize(model.size)}</td>
                  <td className="px-3 py-1.5 text-ide-muted">
                    <button
                      onClick={() => onOpenLocation(model.path)}
                      className="p-1 hover:text-ide-text hover:bg-ide-panel rounded transition-colors"
                      title={model.path}
                    >
                      <ExternalLink className="w-3.5 h-3.5" />
                    </button>
                  </td>
                </tr>
              ))
            ) : (
              <tr>
                <td colSpan="4" className="px-4 py-6 text-center text-ide-muted text-sm">
                  {t('settings.features.models.empty', '暂无模型数据')}
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </section>
  );
}
