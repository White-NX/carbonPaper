import React from 'react';
import { useTranslation } from 'react-i18next';
import { Paperclip } from 'lucide-react';
import SettingsHelpTooltip from '../SettingsHelpTooltip';

export default function AgentAccessHeader() {
  const { t } = useTranslation();

  return (
    <>
      <div className="space-y-1 px-1">
        <h2 className="flex items-center gap-2 text-sm font-semibold text-ide-accent">
          <Paperclip className="h-4 w-4" />
          {t('settings.ai_embedding.title')}{' '}
          <span className="px-1 py-0.5 bg-amber-500/20 text-amber-400 text-[10px] rounded">alpha</span>
          <SettingsHelpTooltip>{t('settings.ai_embedding.mcp_description')}</SettingsHelpTooltip>
        </h2>
        <p className="text-xs text-ide-muted">{t('settings.ai_embedding.description')}</p>
      </div>
    </>
  );
}
