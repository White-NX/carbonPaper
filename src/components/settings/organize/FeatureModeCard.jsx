import React from 'react';
import { useTranslation } from 'react-i18next';
import { ChevronDown } from 'lucide-react';
import { SettingsSegmentedControl, SettingsSwitch } from '../SettingsControls';

export default function FeatureModeCard({
  config,
  featureMode,
  featureModeOptions,
  selectedFeatureMode,
  customControlsOpen,
  scModelAvailable,
  onFeatureModeChange,
  onCustomFeatureToggle,
  onToggleCustomControls,
}) {
  const { t } = useTranslation();

  return (
    <div className="p-4 bg-ide-bg border border-ide-border rounded-xl space-y-3">
      <div>
        <p className="text-sm text-ide-text font-medium">{t('settings.features.management.featureMode.label', '功能等级')}</p>
        <p className="text-xs text-ide-muted mt-1">{t('settings.features.management.featureMode.description', '选择截图语义功能的启用深度')}</p>
      </div>

      <SettingsSegmentedControl
        value={featureMode}
        options={featureModeOptions}
        onChange={onFeatureModeChange}
        density="card"
        className="grid-cols-2 md:grid-cols-4"
      />

      {featureMode !== 'custom' && (
        <p className="text-xs text-ide-muted">{selectedFeatureMode.description}</p>
      )}

      <div className="pt-3 border-t border-ide-border/50">
        <button
          type="button"
          onClick={onToggleCustomControls}
          className={`flex w-full items-center justify-between gap-3 text-left rounded-lg px-2 py-1.5 transition-colors ${
            customControlsOpen
              ? 'bg-ide-panel/70 text-ide-text'
              : 'text-ide-muted hover:bg-ide-hover hover:text-ide-text'
          }`}
        >
          <span className="text-sm font-medium">{t('settings.features.management.featureMode.customControls.label')}</span>
          <ChevronDown className={`w-4 h-4 transition-transform ${customControlsOpen ? 'rotate-180' : ''}`} />
        </button>

        {customControlsOpen && (
          <div className="mt-3 space-y-3 rounded-lg border border-ide-border/70 bg-ide-panel/35 p-3">
            {[
              {
                key: 'classification_enabled',
                label: t('settings.features.management.classification.label', '内容分类'),
                description: t('settings.features.management.classification.description', '使用 BGE 模型自动分类截图内容'),
              },
              {
                key: 'clustering_enabled',
                label: t('settings.features.management.clustering.label', '任务聚类'),
                description: t('settings.features.management.clustering.description', '使用 MiniLM 模型将相似活动分组为长期任务'),
              },
              {
                key: 'smart_cluster_enabled',
                label: t('settings.features.management.smartCluster.label', '智能聚类（按描述自动归档）'),
                description: scModelAvailable
                  ? t('settings.features.management.smartCluster.description', '输入一句话描述（如 "对加利福尼亚山脉的研究"），自动归档相关快照。仅在系统空闲时计算。')
                  : t('settings.features.management.smartCluster.modelMissing', '请先下载模型'),
                disabled: !scModelAvailable && !config.smart_cluster_enabled,
              },
            ].map((item, index) => (
              <div key={item.key} className={index > 0 ? 'border-t border-ide-border/50 pt-3' : ''}>
                <div className="flex items-center justify-between gap-4">
                  <div className="min-w-0">
                    <p className="text-sm font-medium text-ide-text">{item.label}</p>
                    <p className={`mt-1 text-xs ${item.disabled ? 'text-ide-warning-muted' : 'text-ide-muted'}`}>{item.description}</p>
                  </div>
                  <SettingsSwitch
                    checked={Boolean(config[item.key])}
                    onChange={() => onCustomFeatureToggle(item.key)}
                    disabled={item.disabled}
                    title={item.disabled ? t('settings.features.management.smartCluster.modelMissing', '请先下载模型') : item.label}
                  />
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
