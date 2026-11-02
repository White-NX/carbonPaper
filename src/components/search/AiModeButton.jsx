import React from 'react';
import { Bot } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Button } from '../ui/Button';

/** The search field's compact AI entry expands into the selected mode. */
export function AiModeButton({ active, onClick, disabled = false }) {
  const { t } = useTranslation();
  return (
    <Button size="xs" variant={active ? 'primary' : 'ghost'} icon={Bot}
      className="search-ai-mode-button" data-active={active} aria-pressed={active}
      aria-label={t('advancedSearch.modes.ai')} title={t('advancedSearch.modes.ai_hint')}
      onClick={onClick} disabled={disabled}>
      <span className="search-ai-mode-label" aria-hidden="true">{t('advancedSearch.modes.ai')}</span>
    </Button>
  );
}
