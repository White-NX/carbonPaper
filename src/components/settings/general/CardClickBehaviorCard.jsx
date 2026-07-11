import React from 'react';
import { useTranslation } from 'react-i18next';
import DropdownSelect from './DropdownSelect';

export default function CardClickBehaviorCard({
  cardClickBehaviorSearch,
  cardClickBehaviorClusters,
  cardClickBehaviorActivityContext,
  onSetCardClickBehavior,
}) {
  const { t } = useTranslation();
  const options = [
    { value: 'preview', label: t('settings.general.cardClickBehavior.preview') },
    { value: 'standalone', label: t('settings.general.cardClickBehavior.standalone') },
  ];

  return (
    <div className="space-y-4">
      <div>
        <label className="block font-semibold text-ide-text mb-1">{t('settings.general.cardClickBehavior.label')}</label>
        <p className="text-xs text-ide-muted">
          {t('settings.general.cardClickBehavior.description')}
        </p>
      </div>

      <div className="space-y-3 pl-4 border-l-2 border-ide-border">
        <div className="flex items-center justify-between gap-4">
          <label className="text-xs text-ide-text font-medium">{t('settings.general.cardClickBehavior.searchLabel')}</label>
          <DropdownSelect
            value={cardClickBehaviorSearch}
            onChange={(val) => onSetCardClickBehavior('search', val)}
            options={options}
          />
        </div>

        <div className="flex items-center justify-between gap-4">
          <label className="text-xs text-ide-text font-medium">{t('settings.general.cardClickBehavior.clustersLabel')}</label>
          <DropdownSelect
            value={cardClickBehaviorClusters}
            onChange={(val) => onSetCardClickBehavior('clusters', val)}
            options={options}
          />
        </div>

        <div className="flex items-center justify-between gap-4">
          <label className="text-xs text-ide-text font-medium">{t('settings.general.cardClickBehavior.activityContextLabel')}</label>
          <DropdownSelect
            value={cardClickBehaviorActivityContext}
            onChange={(val) => onSetCardClickBehavior('activityContext', val)}
            options={options}
          />
        </div>
      </div>
    </div>
  );
}
