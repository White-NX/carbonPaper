import React from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, RefreshCw } from 'lucide-react';
import { Banner } from '../../ui/Banner';
import { SettingsSwitch } from '../SettingsControls';
import AccessTokenRow from './AccessTokenRow';
import AgentSetupRow from './AgentSetupRow';
import ConnectionInfoRow from './ConnectionInfoRow';
import ContentFilterSection from './ContentFilterSection';
import FilterModeSection from './FilterModeSection';
import McpSmokeTestRow from './McpSmokeTestRow';
import PiiDetectionSection from './PiiDetectionSection';

function RowDivider() {
  return <div className="w-full h-px bg-ide-border/50" />;
}

export default function McpServiceCard({
  enabled,
  port,
  running,
  mode,
  actionLoading,
  restoreLoading,
  error,
  tokenCopied,
  agentPromptCopied,
  diagnosticsCopied,
  agentVariant,
  agentSkill,
  smokeTestLoading,
  smokeTestReport,
  mcpOperationLoading,
  filterEnabled,
  filterCategories,
  filterMode,
  showAdvanced,
  piiEnabled,
  piiEntities,
  piiMaskLongNumbers,
  showPiiAdvanced,
  filterLevel,
  shouldShowStartButton,
  statusBadge,
  statusMessage,
  onStartService,
  onToggle,
  onRequestResetToken,
  onLevelChange,
  onCategoryToggle,
  onFilterModeChange,
  onToggleAdvanced,
  onPiiToggle,
  onPiiEntityToggle,
  onPiiMaskLongNumbersToggle,
  onTogglePiiAdvanced,
  onCopyCurrentToken,
  onAgentVariantChange,
  onCopyAgentSetupPrompt,
  onCopyAgentDiagnostics,
  onRunSmokeTest,
}) {
  const { t } = useTranslation();

  return (
    <div className="p-4 bg-ide-bg border border-ide-border rounded-xl text-sm text-ide-muted space-y-3">
      <div className="flex items-center justify-between gap-4">
        <div>
          <label className="block mb-1 font-semibold text-ide-text">
            {t('settings.ai_embedding.enable_label')}{' '}
            {enabled && (
              <span className={statusBadge.className}>
                {statusBadge.label}
              </span>
            )}
          </label>
          <p className="text-xs text-ide-muted">
            {statusMessage}
          </p>
        </div>
        <div className="flex items-center gap-2 shrink-0">
          {shouldShowStartButton && (
            <button
              onClick={() => onStartService({ auto: false })}
              disabled={mcpOperationLoading}
              className="px-3 py-1.5 text-xs text-ide-text hover:bg-ide-hover border border-ide-border rounded-lg transition-colors flex items-center gap-1.5 whitespace-nowrap disabled:opacity-50"
            >
              {restoreLoading ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : <RefreshCw className="w-3.5 h-3.5" />}
              {restoreLoading ? t('settings.ai_embedding.starting') : t('settings.ai_embedding.start_button')}
            </button>
          )}
          <SettingsSwitch
            title={t('settings.ai_embedding.enable_label')}
            checked={enabled}
            onChange={onToggle}
            disabled={mcpOperationLoading}
          />
        </div>
      </div>

      {error && <Banner tone="error">{error}</Banner>}

      {enabled && mode === 'interactive' && (
        <>
          <RowDivider />
          <AccessTokenRow
            tokenCopied={tokenCopied}
            actionLoading={mcpOperationLoading}
            onCopyCurrentToken={onCopyCurrentToken}
            onRequestResetToken={onRequestResetToken}
          />
        </>
      )}

      {enabled && mode === 'interactive' && (
        <>
          <RowDivider />
          <McpSmokeTestRow
            loading={smokeTestLoading}
            disabled={mcpOperationLoading}
            report={smokeTestReport}
            onRun={onRunSmokeTest}
          />
        </>
      )}

      {enabled && (
        <>
          <RowDivider />
          <AgentSetupRow
            port={port}
            mode={mode}
            agentSkill={agentSkill}
            agentVariant={agentVariant}
            agentPromptCopied={agentPromptCopied}
            diagnosticsCopied={diagnosticsCopied}
            onAgentVariantChange={onAgentVariantChange}
            onCopyAgentSetupPrompt={onCopyAgentSetupPrompt}
            onCopyDiagnostics={onCopyAgentDiagnostics}
          />
        </>
      )}

      {enabled && running && (
        <>
          <RowDivider />
          <ConnectionInfoRow port={port} mode={mode} />
        </>
      )}

      {enabled && (
        <>
          <RowDivider />
          <ContentFilterSection
            filterLevel={filterLevel}
            filterEnabled={filterEnabled}
            filterCategories={filterCategories}
            showAdvanced={showAdvanced}
            onToggleAdvanced={onToggleAdvanced}
            onLevelChange={onLevelChange}
            onCategoryToggle={onCategoryToggle}
          />
        </>
      )}

      {enabled && (filterEnabled || piiEnabled) && (
        <>
          <RowDivider />
          <FilterModeSection
            filterMode={filterMode}
            onFilterModeChange={onFilterModeChange}
          />
        </>
      )}

      {enabled && (
        <>
          <RowDivider />
          <PiiDetectionSection
            piiEnabled={piiEnabled}
            piiEntities={piiEntities}
            piiMaskLongNumbers={piiMaskLongNumbers}
            showPiiAdvanced={showPiiAdvanced}
            onPiiToggle={onPiiToggle}
            onTogglePiiAdvanced={onTogglePiiAdvanced}
            onPiiEntityToggle={onPiiEntityToggle}
            onPiiMaskLongNumbersToggle={onPiiMaskLongNumbersToggle}
          />
        </>
      )}
    </div>
  );
}
