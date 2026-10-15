import React from 'react';
import { useTranslation } from 'react-i18next';
import { FileText } from 'lucide-react';
import { Button } from '../ui/Button';

/** Facts remain individually citable; a claim with missing evidence stays hidden. */
export default function RecapKeyDetails({ details = [], sources = [], onSources }) {
  const { t } = useTranslation();
  const supported = details.filter((detail) => detail.evidence_ids?.length
    && detail.evidence_ids.every((id) => sources.some((source) => source.id === id)));
  if (!supported.length) return null;
  return <section className="mt-5 rounded-xl border border-ide-border bg-ide-panel/60 px-4 py-4" aria-label={t('recap.keyDetails')}>
    <h3 className="mb-3 text-xs font-semibold text-ide-text">{t('recap.keyDetails')}</h3>
    <ul className="space-y-3">
      {supported.map((detail, index) => <li key={`${index}:${detail.text}`} className="flex items-start gap-3">
        <span aria-hidden="true" className="mt-2 h-1.5 w-1.5 shrink-0 rounded-full bg-ide-accent/60" />
        <div className="min-w-0 flex-1">
          <p className="whitespace-pre-wrap break-words text-[13px] leading-6">{detail.text}</p>
          {onSources && <Button variant="ghost" size="xs" icon={FileText} className="mt-1 -ml-2" onClick={() => onSources(sources.filter((source) => detail.evidence_ids.includes(source.id)))}>
            {t('recap.detailSources', { count: detail.evidence_ids.length })}
          </Button>}
        </div>
      </li>)}
    </ul>
  </section>;
}
