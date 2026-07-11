import React from 'react';
import { useTranslation } from 'react-i18next';
import { useAiEmbeddingController } from '../useAiEmbeddingController';
import AgentAccessDialogs from './AgentAccessDialogs';
import AgentAccessHeader from './AgentAccessHeader';
import { AGENT_SKILL_NAME, AGENT_SKILL_REPO } from './agentAccessConstants';
import McpServiceCard from './McpServiceCard';

export default function AgentAccessSection() {
  const { t } = useTranslation();
  const {
    enabled,
    port,
    running,
    loading,
    actionLoading,
    restoreLoading,
    error,
    showPrivacyDialog,
    setShowPrivacyDialog,
    confirmText,
    setConfirmText,
    tokenCopied,
    agentPromptCopied,
    showResetConfirm,
    setShowResetConfirm,
    filterEnabled,
    filterCategories,
    filterMode,
    showAdvanced,
    setShowAdvanced,
    piiEnabled,
    piiEntities,
    spacyModels,
    downloadingModel,
    recheckLoading,
    showPiiAdvanced,
    setShowPiiAdvanced,
    CONFIRM_TEXT,
    startMcpService,
    handleToggle,
    handleConfirmEnable,
    handleResetToken,
    filterLevel,
    handleLevelChange,
    handleCategoryToggle,
    handleFilterModeChange,
    handlePiiToggle,
    handlePiiEntityToggle,
    handleDownloadModel,
    handleForceRecheck,
    handleCopyCurrentToken,
    handleCopyAgentSetupPrompt,
    shouldShowStartButton,
    statusBadge,
    statusMessage,
  } = useAiEmbeddingController({
    t,
    agentSkillName: AGENT_SKILL_NAME,
    agentSkillRepo: AGENT_SKILL_REPO,
  });

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <div className="w-5 h-5 border-2 border-ide-accent border-t-transparent rounded-full animate-spin" />
      </div>
    );
  }

  return (
    <div className="space-y-3">
      <AgentAccessHeader />

      <McpServiceCard
        enabled={enabled}
        port={port}
        running={running}
        actionLoading={actionLoading}
        restoreLoading={restoreLoading}
        error={error}
        tokenCopied={tokenCopied}
        agentPromptCopied={agentPromptCopied}
        filterEnabled={filterEnabled}
        filterCategories={filterCategories}
        filterMode={filterMode}
        showAdvanced={showAdvanced}
        piiEnabled={piiEnabled}
        piiEntities={piiEntities}
        spacyModels={spacyModels}
        downloadingModel={downloadingModel}
        recheckLoading={recheckLoading}
        showPiiAdvanced={showPiiAdvanced}
        filterLevel={filterLevel}
        shouldShowStartButton={shouldShowStartButton}
        statusBadge={statusBadge}
        statusMessage={statusMessage}
        onStartService={startMcpService}
        onToggle={handleToggle}
        onRequestResetToken={() => setShowResetConfirm(true)}
        onLevelChange={handleLevelChange}
        onCategoryToggle={handleCategoryToggle}
        onFilterModeChange={handleFilterModeChange}
        onToggleAdvanced={() => setShowAdvanced(!showAdvanced)}
        onPiiToggle={handlePiiToggle}
        onPiiEntityToggle={handlePiiEntityToggle}
        onDownloadModel={handleDownloadModel}
        onForceRecheck={handleForceRecheck}
        onTogglePiiAdvanced={() => setShowPiiAdvanced(!showPiiAdvanced)}
        onCopyCurrentToken={handleCopyCurrentToken}
        onCopyAgentSetupPrompt={handleCopyAgentSetupPrompt}
      />

      <AgentAccessDialogs
        showPrivacyDialog={showPrivacyDialog}
        onClosePrivacyDialog={() => setShowPrivacyDialog(false)}
        confirmText={confirmText}
        onConfirmTextChange={setConfirmText}
        confirmTextExpected={CONFIRM_TEXT}
        onConfirmEnable={handleConfirmEnable}
        showResetConfirm={showResetConfirm}
        onCloseResetConfirm={() => setShowResetConfirm(false)}
        onResetToken={handleResetToken}
      />
    </div>
  );
}
