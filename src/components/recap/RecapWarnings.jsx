import React from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle } from 'lucide-react';
import { Banner } from '../ui/Banner';

export default function RecapWarnings({ warnings, compact = false, className = '' }) {
  const { t } = useTranslation();
  if (!warnings?.length) return null;
  const text = t('recap.unverifiedContent');
  if (compact) return <span className={`mt-2 flex items-start gap-1.5 text-xs leading-relaxed text-ide-warning ${className}`}>
    <AlertTriangle aria-hidden="true" className="mt-0.5 h-3.5 w-3.5 shrink-0" /><span>{text}</span>
  </span>;
  return <Banner tone="warning" className={className}>{text}</Banner>;
}
