import React from 'react';
import { useTranslation } from 'react-i18next';
import { Paperclip } from 'lucide-react';
import SettingsHelpTooltip from '../SettingsHelpTooltip';

export default function AgentAccessHeader() {
  const { t } = useTranslation();

  return (
    <>
      <div className="space-y-1">
        <h2 className="text-xl font-semibold">
          {t('settings.ai_embedding.title')}{' '}
          <span className="px-1 py-0.5 bg-amber-500/20 text-amber-400 text-[10px] rounded">alpha</span>
        </h2>
        <p className="text-xs text-ide-muted">{t('settings.ai_embedding.description')}</p>
      </div>

      <div className="flex items-center gap-1.5 px-1">
        <Paperclip className="w-4 h-4 text-ide-accent" />
        <label className="text-sm font-semibold text-ide-accent block">{t('settings.ai_embedding.mcp_service')}</label>
        <SettingsHelpTooltip>{t('settings.ai_embedding.mcp_description')}</SettingsHelpTooltip>
      </div>
    </>
  );
}
