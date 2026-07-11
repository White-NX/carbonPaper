import React from 'react';
import { useTranslation } from 'react-i18next';
import { Check, Copy } from 'lucide-react';
import { AGENT_SKILL_NAME, AGENT_SKILL_REPO } from './agentAccessConstants';

export default function AgentSetupRow({
  port,
  agentPromptCopied,
  onCopyAgentSetupPrompt,
}) {
  const { t } = useTranslation();

  return (
    <div className="flex items-center justify-between gap-4">
      <div className="min-w-0 flex-1">
        <label className="block mb-1 font-semibold text-ide-text">{t('settings.ai_embedding.agent_setup.title')}</label>
        <div className="space-y-1.5">
          <p className="text-xs text-ide-muted">{t('settings.ai_embedding.agent_setup.description')}</p>
          <div className="grid gap-1.5 text-xs">
            <div className="min-w-0">
              <span className="text-ide-muted">{t('settings.ai_embedding.agent_setup.skill')}</span>{' '}
              <code className="text-ide-text font-mono">{AGENT_SKILL_NAME}</code>
            </div>
            <div className="min-w-0">
              <span className="text-ide-muted">{t('settings.ai_embedding.agent_setup.source')}</span>{' '}
              <code className="break-all text-ide-text font-mono">{AGENT_SKILL_REPO}</code>
            </div>
            <div className="min-w-0">
              <span className="text-ide-muted">{t('settings.ai_embedding.agent_setup.endpoint')}</span>{' '}
              <code className="break-all text-ide-text font-mono">POST http://localhost:{port}/mcp</code>
            </div>
            <div className="min-w-0">
              <span className="text-ide-muted">{t('settings.ai_embedding.connection_info.auth_header')}</span>{' '}
              <code className="break-all text-ide-text font-mono">Authorization: Bearer &lt;CarbonPaper token&gt;</code>
            </div>
          </div>
          {agentPromptCopied && (
            <p className="text-xs text-green-400">{t('settings.ai_embedding.agent_setup.copied')}</p>
          )}
        </div>
      </div>
      <button
        onClick={onCopyAgentSetupPrompt}
        className="shrink-0 px-3 py-1.5 text-xs text-ide-text hover:bg-ide-hover border border-ide-border rounded-lg transition-colors flex items-center gap-1.5"
      >
        {agentPromptCopied ? <Check className="w-3.5 h-3.5 text-green-400" /> : <Copy className="w-3.5 h-3.5" />}
        {t('settings.ai_embedding.agent_setup.copy')}
      </button>
    </div>
  );
}
