import React from 'react';
import { useTranslation } from 'react-i18next';
import { Check, Copy } from 'lucide-react';
import { SettingsButton, SettingsSegmentedControl } from '../SettingsControls';
import { AGENT_SETUP_VARIANTS } from './agentAccessConstants';

const AGENT_THEME_CLASSES = {
  codex: {
    selectedClassName: 'bg-zinc-950 text-white hover:bg-zinc-900',
    idleClassName: 'text-zinc-700 hover:bg-zinc-950/10 hover:text-zinc-950 dark:text-zinc-300 dark:hover:bg-zinc-100/10 dark:hover:text-white',
  },
  claude: {
    selectedClassName: 'bg-[#D97757] text-white hover:bg-[#C76645]',
    idleClassName: 'text-[#B4532F] hover:bg-[#D97757]/10 hover:text-[#9A3F22] dark:text-[#E7A185] dark:hover:bg-[#D97757]/20 dark:hover:text-[#F2B39E]',
  },
  cursor: {
    selectedClassName: 'bg-[#4F8CFF] text-white hover:bg-[#3D7DF0]',
    idleClassName: 'text-[#3268C5] hover:bg-[#4F8CFF]/10 hover:text-[#2455A6] dark:text-[#8EB5FF] dark:hover:bg-[#4F8CFF]/20 dark:hover:text-[#B7D0FF]',
  },
  generic: {
    selectedClassName: 'bg-slate-600 text-white hover:bg-slate-500',
    idleClassName: 'text-slate-600 hover:bg-slate-600/10 hover:text-slate-700 dark:text-slate-300 dark:hover:bg-slate-400/15 dark:hover:text-slate-100',
  },
};

export default function AgentSetupRow({
  port,
  agentSkill,
  agentVariant,
  agentPromptCopied,
  diagnosticsCopied,
  onAgentVariantChange,
  onCopyAgentSetupPrompt,
  onCopyDiagnostics,
}) {
  const { t } = useTranslation();
  const variantOptions = AGENT_SETUP_VARIANTS.map((variant) => ({
    value: variant,
    label: t(`settings.ai_embedding.agent_setup.variants.${variant}.label`),
    title: t(`settings.ai_embedding.agent_setup.variants.${variant}.description`),
    ...AGENT_THEME_CLASSES[variant],
  }));

  return (
    <div className="space-y-3">
      <div className="min-w-0">
        <label className="block mb-1 font-semibold text-ide-text">
          {t('settings.ai_embedding.agent_setup.title')}
        </label>
        <p className="text-xs text-ide-muted">
          {t('settings.ai_embedding.agent_setup.description')}
        </p>
      </div>

      <SettingsSegmentedControl
        value={agentVariant}
        options={variantOptions}
        columns={4}
        onChange={onAgentVariantChange}
      />

      <p className="text-xs text-ide-muted">
        {t(`settings.ai_embedding.agent_setup.variants.${agentVariant}.description`)}
      </p>

      <div className="grid gap-1.5 text-xs">
        <div className="min-w-0">
          <span className="text-ide-muted">{t('settings.ai_embedding.agent_setup.skill')}</span>{' '}
          <code className="text-ide-text font-mono">{agentSkill.id}</code>
          {agentSkill.tool_schema_version != null && (
            <span className="ml-2 text-ide-muted">
              {t('settings.ai_embedding.agent_setup.schema_version', {
                version: agentSkill.tool_schema_version,
              })}
            </span>
          )}
        </div>
        <div className="min-w-0">
          <span className="text-ide-muted">{t('settings.ai_embedding.agent_setup.source')}</span>{' '}
          <code className="break-all text-ide-text font-mono">{agentSkill.source_repository}</code>
        </div>
        <div className="min-w-0">
          <span className="text-ide-muted">{t('settings.ai_embedding.agent_setup.endpoint')}</span>{' '}
          <code className="break-all text-ide-text font-mono">POST http://127.0.0.1:{port}/mcp</code>
        </div>
        <div className="min-w-0">
          <span className="text-ide-muted">{t('settings.ai_embedding.connection_info.auth_header')}</span>{' '}
          <code className="break-all text-ide-text font-mono">
            Authorization: Bearer &lt;CarbonPaper token&gt;
          </code>
        </div>
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <SettingsButton
          icon={agentPromptCopied ? <Check className="w-3.5 h-3.5 text-green-400" /> : Copy}
          onClick={onCopyAgentSetupPrompt}
        >
          {t('settings.ai_embedding.agent_setup.copy')}
        </SettingsButton>
        <SettingsButton
          icon={diagnosticsCopied ? <Check className="w-3.5 h-3.5 text-green-400" /> : Copy}
          onClick={onCopyDiagnostics}
        >
          {t('settings.ai_embedding.agent_setup.copy_diagnostics')}
        </SettingsButton>
        {agentPromptCopied && (
          <span className="text-xs text-green-400">
            {t('settings.ai_embedding.agent_setup.copied')}
          </span>
        )}
      </div>
    </div>
  );
}
