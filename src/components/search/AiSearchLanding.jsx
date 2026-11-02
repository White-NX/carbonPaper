import React, { useId } from 'react';
import { ArrowUpRight, Loader2, Settings } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Button } from '../ui/Button';
import { Banner } from '../ui/Banner';
import { openSettingsWindow } from '../../lib/settings_api';

/** Questions introduce the history that AI search can help the user revisit. */
export function AiSearchLanding({ onChooseQuestion, selectedQuestion = '', loading = false, needsSetup = false }) {
  const { t } = useTranslation();
  const titleId = useId();
  const suggestions = [
    { id: 'day', label: t('aiSearch.landing.cards.day.label'), title: t('aiSearch.landing.cards.day.title'),
      description: t('aiSearch.landing.cards.day.description'), question: t('aiSearch.landing.cards.day.question') },
    { id: 'reading', label: t('aiSearch.landing.cards.reading.label'), title: t('aiSearch.landing.cards.reading.title'),
      description: t('aiSearch.landing.cards.reading.description'), question: t('aiSearch.landing.cards.reading.question') },
    { id: 'discussion', label: t('aiSearch.landing.cards.discussion.label'), title: t('aiSearch.landing.cards.discussion.title'),
      description: t('aiSearch.landing.cards.discussion.description'), question: t('aiSearch.landing.cards.discussion.question') },
    { id: 'project', label: t('aiSearch.landing.cards.project.label'), title: t('aiSearch.landing.cards.project.title'),
      description: t('aiSearch.landing.cards.project.description'), question: t('aiSearch.landing.cards.project.question') },
  ];

  return (
    <section className="ai-search-landing" aria-labelledby={titleId}>
      <div className="ai-search-landing-heading">
        <h2 id={titleId} className="text-xl font-medium text-ide-text">{t('aiSearch.landing.title')}</h2>
        <p className="mt-1 text-xs text-ide-muted">{t('aiSearch.landing.description')}</p>
      </div>
      {loading && <p role="status" className="mb-5 flex items-center gap-2 text-xs text-ide-muted">
        <Loader2 className="h-3.5 w-3.5 animate-spin motion-reduce:animate-none" aria-hidden="true" />
        {t('aiSearch.landing.preparing')}
      </p>}
      {needsSetup && <Banner icon={Settings} className="ai-search-setup-banner mb-5" title={t('aiSearch.setup.title')}
        action={<Button size="xs" onClick={() => openSettingsWindow('ai', 'ai-providers')}>{t('aiSearch.setup.button')}</Button>}>
        {t('aiSearch.setup.description')}
      </Banner>}
      <div className="ai-search-landing-grid">
        {suggestions.map((card, index) => (
          <Button key={card.id} variant="ghost" className="ai-search-suggestion" data-topic={card.id}
            style={{ '--search-landing-order': index }} aria-label={card.question}
            aria-pressed={selectedQuestion === card.question} onClick={() => onChooseQuestion(card.question)}>
            <ArrowUpRight className="ai-search-suggestion-arrow" strokeWidth={1.1} aria-hidden="true" />
            <span className="ai-search-suggestion-label">{card.label}</span>
            <span className="ai-search-suggestion-title">{card.title}</span>
            <span className="ai-search-suggestion-footer">
              <span>{card.description}</span><ArrowUpRight className="h-4 w-4 shrink-0" aria-hidden="true" />
            </span>
          </Button>
        ))}
      </div>
    </section>
  );
}
