import React from 'react';
import { useTranslation } from 'react-i18next';
import BrandMark from './BrandMark';

export default function EmptyPreview() {
  const { t } = useTranslation();

  return (
    <div className="main-preview-brand pointer-events-none absolute text-left select-none">
      <BrandMark className="main-preview-brand-mark text-ide-muted" />
      <div className="min-w-0">
        <h1 className="main-preview-brand-name font-semibold tracking-tight text-ide-text">CarbonPaper</h1>
        <p className="mt-3 text-sm leading-relaxed text-ide-muted">{t('mainPreview.emptyHint')}</p>
      </div>
    </div>
  );
}
