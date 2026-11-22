import React from 'react';
import { useTranslation } from 'react-i18next';
import { recapBudgetIssue } from '../../lib/recap_api';
import { Banner } from '../ui/Banner';
import { Button } from '../ui/Button';

export default function RecapBudgetNotice({ error, target, onAdjust, className = '' }) {
  const { t, i18n } = useTranslation();
  const issue = recapBudgetIssue(error);
  if (!issue) return null;
  const detailed = issue.requested != null;
  return <Banner tone="warning" className={className} action={onAdjust && <Button variant="ghost" size="xs" onClick={onAdjust}>{t('recap.adjustBudget')}</Button>}>
    {t(`recap.budgetNotice.${issue.scope}${detailed ? 'Detail' : ''}`, {
      target: t(target), requested: issue.requested?.toLocaleString(i18n?.language), available: issue.available?.toLocaleString(i18n?.language),
    })}
  </Banner>;
}
