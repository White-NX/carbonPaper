import React from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, Copy, Check, RefreshCw, ChevronDown, ChevronUp, Download, Loader2, Paperclip } from 'lucide-react';
import { Dialog } from '../Dialog';
import { ConfirmDialog } from '../ConfirmDialog';
import SettingsHelpTooltip from './SettingsHelpTooltip';
import { SettingsSwitch } from './SettingsControls';
import { useAiEmbeddingController } from './useAiEmbeddingController';

const AGENT_SKILL_NAME = 'carbonpaper-memory';
const AGENT_SKILL_REPO = 'https://github.com/White-NX/carbonPaperSkill';

export default function AiEmbeddingSection() {
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
      {/* Header — kept as-is */}
      <div className="space-y-1">
        <h2 className="text-xl font-semibold">{t('settings.ai_embedding.title')} <span className="px-1 py-0.5 bg-amber-500/20 text-amber-400 text-[10px] rounded">alpha</span></h2>
        <p className="text-xs text-ide-muted">{t('settings.ai_embedding.description')}</p>
      </div>

      <div className="flex items-center gap-1.5 px-1">
        <Paperclip className="w-4 h-4 text-ide-accent" />
        <label className="text-sm font-semibold text-ide-accent block">{t('settings.ai_embedding.mcp_service')}</label>
        <SettingsHelpTooltip>{t('settings.ai_embedding.mcp_description')}</SettingsHelpTooltip>
      </div>

      {/* Options card — single card with dividers, matching MonitorServiceSection */}
      <div className="p-4 bg-ide-bg border border-ide-border rounded-xl text-sm text-ide-muted space-y-3">
        {/* Row 1: Enable toggle + status */}
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
                onClick={() => startMcpService({ auto: false })}
                disabled={actionLoading || restoreLoading}
                className="px-3 py-1.5 text-xs text-ide-text hover:bg-ide-hover border border-ide-border rounded-lg transition-colors flex items-center gap-1.5 whitespace-nowrap disabled:opacity-50"
              >
                {restoreLoading ? <Loader2 className="w-3.5 h-3.5 animate-spin" /> : <RefreshCw className="w-3.5 h-3.5" />}
                {restoreLoading ? t('settings.ai_embedding.starting') : t('settings.ai_embedding.start_button')}
              </button>
            )}
            <SettingsSwitch
              checked={enabled}
              onChange={handleToggle}
              disabled={actionLoading || restoreLoading}
            />
          </div>
        </div>

        {error && (
          <div className="p-3 bg-red-500/10 border border-red-500/30 rounded-lg">
            <p className="text-xs text-red-400">{error}</p>
          </div>
        )}

        {/* Row 2: Token management */}
        {enabled && (
          <>
            <div className="w-full h-px bg-ide-border/50" />
            <div className="flex items-center justify-between gap-4">
              <div className="flex-1">
                <label className="block mb-1 font-semibold text-ide-text">{t('settings.ai_embedding.token.title')}</label>
                <p className="text-xs text-ide-muted">{t('settings.ai_embedding.token.description')}</p>
                {tokenCopied && (
                  <p className="text-xs text-green-400 mt-1">{t('settings.ai_embedding.token.copied')}</p>
                )}
              </div>
              <button
                onClick={handleCopyCurrentToken}
                disabled={actionLoading}
                className="shrink-0 px-3 py-1.5 text-xs text-ide-text hover:bg-ide-hover border border-ide-border rounded-lg transition-colors flex items-center gap-1.5 disabled:opacity-50"
              >
                {tokenCopied ? <Check className="w-3.5 h-3.5 text-green-400" /> : <Copy className="w-3.5 h-3.5" />}
                {t('settings.ai_embedding.token.copy')}
              </button>
              <button
                onClick={() => setShowResetConfirm(true)}
                disabled={actionLoading}
                className="shrink-0 px-3 py-1.5 text-xs text-red-400 hover:text-red-300 hover:bg-red-500/10 border border-red-500/30 rounded-lg transition-colors flex items-center gap-1.5 disabled:opacity-50"
              >
                <RefreshCw className="w-3.5 h-3.5" />
                {t('settings.ai_embedding.token.reset')}
              </button>
            </div>
          </>
        )}

        {/* Row 3: Agent setup */}
        {enabled && (
          <>
            <div className="w-full h-px bg-ide-border/50" />
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
                onClick={handleCopyAgentSetupPrompt}
                className="shrink-0 px-3 py-1.5 text-xs text-ide-text hover:bg-ide-hover border border-ide-border rounded-lg transition-colors flex items-center gap-1.5"
              >
                {agentPromptCopied ? <Check className="w-3.5 h-3.5 text-green-400" /> : <Copy className="w-3.5 h-3.5" />}
                {t('settings.ai_embedding.agent_setup.copy')}
              </button>
            </div>
          </>
        )}

        {/* Row 4: Connection info */}
        {enabled && running && (
          <>
            <div className="w-full h-px bg-ide-border/50" />
            <div>
              <label className="block mb-1 font-semibold text-ide-text">{t('settings.ai_embedding.connection_info.title')}</label>
              <div className="space-y-1.5 mt-2">
                <div>
                  <p className="text-xs text-ide-muted">{t('settings.ai_embedding.connection_info.endpoint')}</p>
                  <code className="text-xs text-ide-text font-mono">POST http://localhost:{port}/mcp</code>
                </div>
                <div>
                  <p className="text-xs text-ide-muted">{t('settings.ai_embedding.connection_info.auth_header')}</p>
                  <code className="text-xs text-ide-text font-mono">Authorization: Bearer &lt;your-token&gt;</code>
                </div>
              </div>
            </div>
          </>
        )}

        {/* Row 4: Privacy protection level */}
        {enabled && (
          <>
            <div className="w-full h-px bg-ide-border/50" />
            <div>
              <label className="block mb-2 font-semibold text-ide-text">
                {t('settings.ai_embedding.content_filter.level_label')}
              </label>
              <div className="space-y-1">
                {['standard', 'minimal', 'off'].map((level) => (
                  <button
                    key={level}
                    onClick={() => handleLevelChange(level)}
                    className={`w-full flex items-start gap-3 px-3 py-2 rounded-lg text-left transition-colors ${
                      filterLevel === level
                        ? 'bg-ide-accent/10'
                        : 'hover:bg-ide-hover'
                    }`}
                  >
                    <div className={`mt-1 w-4 h-4 rounded-full border-2 shrink-0 flex items-center justify-center ${
                      filterLevel === level
                        ? 'border-ide-accent'
                        : 'border-ide-muted'
                    }`}>
                      {filterLevel === level && (
                        <div className="w-2 h-2 rounded-full bg-ide-accent" />
                      )}
                    </div>
                    <div>
                      <span className="text-ide-text font-semibold text-sm">
                        {t(`settings.ai_embedding.content_filter.levels.${level}.title`)}
                      </span>
                      <p className="text-ide-muted text-xs mt-0.5">
                        {t(`settings.ai_embedding.content_filter.levels.${level}.description`)}
                      </p>
                    </div>
                  </button>
                ))}
                {filterLevel === 'custom' && (
                  <div className="flex items-start gap-3 px-3 py-2 rounded-lg bg-ide-accent/10">
                    <div className="mt-1 w-4 h-4 rounded-full border-2 border-ide-accent shrink-0 flex items-center justify-center">
                      <div className="w-2 h-2 rounded-full bg-ide-accent" />
                    </div>
                    <div>
                      <span className="text-ide-text font-semibold text-sm">
                        {t('settings.ai_embedding.content_filter.levels.custom.title')}
                      </span>
                      <p className="text-ide-muted text-xs mt-0.5">
                        {t('settings.ai_embedding.content_filter.levels.custom.description')}
                      </p>
                    </div>
                  </div>
                )}
              </div>

              {/* Advanced options toggle */}
              <button
                onClick={() => setShowAdvanced(!showAdvanced)}
                className="flex items-center gap-1 mt-3 text-xs text-ide-muted hover:text-ide-text transition-colors"
              >
                {showAdvanced ? <ChevronUp className="w-3.5 h-3.5" /> : <ChevronDown className="w-3.5 h-3.5" />}
                {t('settings.ai_embedding.content_filter.advanced_toggle')}
              </button>

              {/* Advanced: per-category checkboxes */}
              {showAdvanced && (
                <div className="grid grid-cols-2 gap-2 pl-1 mt-2">
                  {['cat_01', 'cat_02', 'cat_03', 'cat_04', 'cat_05'].map((cat) => (
                    <label key={cat} className="flex items-center gap-2 cursor-pointer text-xs text-ide-text">
                      <input
                        type="checkbox"
                        checked={filterCategories[cat] ?? true}
                        onChange={() => handleCategoryToggle(cat)}
                        disabled={!filterEnabled}
                        className="w-3.5 h-3.5 rounded border-ide-border text-ide-accent focus:ring-ide-accent/50 bg-ide-panel disabled:opacity-50"
                      />
                      {t(`settings.ai_embedding.content_filter.categories.${cat}`)}
                    </label>
                  ))}
                </div>
              )}
            </div>
          </>
        )}

        {/* Row 5: Filter mode */}
        {enabled && filterEnabled && (
          <>
            <div className="w-full h-px bg-ide-border/50" />
            <div>
              <label className="block mb-2 font-semibold text-ide-text">
                {t('settings.ai_embedding.content_filter.filter_mode.label')}
              </label>
              <div className="space-y-1">
                {['reject', 'remove_paragraph', 'mask'].map((mode) => (
                  <button
                    key={mode}
                    onClick={() => handleFilterModeChange(mode)}
                    className={`w-full flex items-start gap-3 px-3 py-2 rounded-lg text-left transition-colors ${
                      filterMode === mode
                        ? 'bg-ide-accent/10'
                        : 'hover:bg-ide-hover'
                    }`}
                  >
                    <div className={`mt-1 w-4 h-4 rounded-full border-2 shrink-0 flex items-center justify-center ${
                      filterMode === mode
                        ? 'border-ide-accent'
                        : 'border-ide-muted'
                    }`}>
                      {filterMode === mode && (
                        <div className="w-2 h-2 rounded-full bg-ide-accent" />
                      )}
                    </div>
                    <div>
                      <span className="text-ide-text font-semibold text-sm">
                        {t(`settings.ai_embedding.content_filter.filter_mode.${mode}.title`)}
                      </span>
                      <p className="text-ide-muted text-xs mt-0.5">
                        {t(`settings.ai_embedding.content_filter.filter_mode.${mode}.description`)}
                      </p>
                    </div>
                  </button>
                ))}
              </div>
            </div>
          </>
        )}

        {/* Row 6: PII Detection */}
        {enabled && (
          <>
            <div className="w-full h-px bg-ide-border/50" />
            <div>
              <div className="flex items-center justify-between gap-4 mb-2">
                <div>
                  <label className="block font-semibold text-ide-text">
                    {t('settings.ai_embedding.content_filter.pii.title')}
                  </label>
                  <p className="text-xs text-ide-muted mt-0.5">
                    {t('settings.ai_embedding.content_filter.pii.description')}
                  </p>
                </div>
                <SettingsSwitch
                  checked={piiEnabled}
                  onChange={handlePiiToggle}
                />
              </div>

              {piiEnabled && (
                <div className="space-y-2 mt-3">
                  {/* Model status */}
                  <div className="space-y-1.5">
                    <div className="flex items-center justify-between">
                      <p className="text-xs font-medium text-ide-text">{t('settings.ai_embedding.content_filter.pii.model_status')}</p>
                      <button
                        onClick={handleForceRecheck}
                        disabled={recheckLoading}
                        className="flex items-center gap-1 px-2 py-1 text-[10px] text-ide-muted hover:text-ide-text hover:bg-ide-hover rounded transition-colors disabled:opacity-50"
                      >
                        <RefreshCw className={`w-3 h-3 ${recheckLoading ? 'animate-spin' : ''}`} />
                        {t('settings.ai_embedding.content_filter.pii.recheck')}
                      </button>
                    </div>
                    {[
                      { key: 'zh_core_web_sm', label: 'zh_core_web_sm', lang: 'zh' },
                      { key: 'en_core_web_sm', label: 'en_core_web_sm', lang: 'en' },
                    ].map(({ key, label, lang }) => {
                      const currentLang = localStorage.getItem('language') || 'zh-CN';
                      const isActive = (currentLang.startsWith('zh') && lang === 'zh') ||
                                       (!currentLang.startsWith('zh') && lang === 'en');
                      const installed = spacyModels[key]?.installed;
                      const isDownloading = downloadingModel === key;
                      return (
                        <div key={key} className="flex items-center justify-between px-3 py-1.5 bg-ide-panel rounded-lg">
                          <div className="flex items-center gap-2 min-w-0">
                            <span className="text-xs text-ide-text">{label}</span>
                            {isActive && (
                              <span className="px-1.5 py-0.5 bg-ide-accent/20 text-ide-accent text-[10px] rounded shrink-0">
                                {t('settings.ai_embedding.content_filter.pii.model_active')}
                              </span>
                            )}
                          </div>
                          <div className="flex items-center gap-2 shrink-0">
                            {installed ? (
                              <span className="text-xs text-green-400">
                                {t('settings.ai_embedding.content_filter.pii.model_installed')}
                              </span>
                            ) : isDownloading ? (
                              <span className="flex items-center gap-1 text-xs text-ide-muted">
                                <Loader2 className="w-3 h-3 animate-spin" />
                                {t('settings.ai_embedding.content_filter.pii.model_downloading')}
                              </span>
                            ) : (
                              <button
                                onClick={() => handleDownloadModel(key)}
                                disabled={!!downloadingModel}
                                className="flex items-center gap-1 px-2 py-1 text-xs text-ide-accent hover:bg-ide-accent/10 rounded transition-colors disabled:opacity-50"
                              >
                                <Download className="w-3 h-3" />
                                {t('settings.ai_embedding.content_filter.pii.model_download')}
                              </button>
                            )}
                          </div>
                        </div>
                      );
                    })}
                    <p className="text-[11px] text-ide-muted px-1">
                      {t('settings.ai_embedding.content_filter.pii.model_note')}
                    </p>
                  </div>

                  {/* Entity types toggle */}
                  <button
                    onClick={() => setShowPiiAdvanced(!showPiiAdvanced)}
                    className="flex items-center gap-1 text-xs text-ide-muted hover:text-ide-text transition-colors"
                  >
                    {showPiiAdvanced ? <ChevronUp className="w-3.5 h-3.5" /> : <ChevronDown className="w-3.5 h-3.5" />}
                    {t('settings.ai_embedding.content_filter.pii.entity_types_label')}
                  </button>

                  {showPiiAdvanced && (
                    <div className="grid grid-cols-2 gap-2 pl-1">
                      {Object.keys(piiEntities).map((entityType) => (
                        <label key={entityType} className="flex items-center gap-2 cursor-pointer text-xs text-ide-text">
                          <input
                            type="checkbox"
                            checked={piiEntities[entityType]}
                            onChange={() => handlePiiEntityToggle(entityType)}
                            className="w-3.5 h-3.5 rounded border-ide-border text-ide-accent focus:ring-ide-accent/50 bg-ide-panel"
                          />
                          {t(`settings.ai_embedding.content_filter.pii.entity_types.${entityType}`)}
                        </label>
                      ))}
                    </div>
                  )}
                </div>
              )}
            </div>
          </>
        )}
      </div>

      {/* Privacy warning dialog */}
      <Dialog
        isOpen={showPrivacyDialog}
        onClose={() => setShowPrivacyDialog(false)}
        title={t('settings.ai_embedding.privacy_warning.title')}
        maxWidth="max-w-md"
      >
        <div className="p-4 space-y-4">
          <div className="p-3 bg-ide-warning-bg border border-ide-warning-border rounded-lg flex items-start gap-2">
            <AlertTriangle className="w-5 h-5 text-ide-warning shrink-0 mt-0.5" />
            <p className="text-xs text-ide-text leading-relaxed whitespace-pre-line">
              {t('settings.ai_embedding.privacy_warning.message')}
            </p>
          </div>

          <div className="space-y-2">
            <p className="text-xs text-ide-muted">
              {t('settings.ai_embedding.privacy_warning.confirm_prompt')}
            </p>
            <p className="text-xs text-ide-text font-medium px-2 py-1.5 bg-ide-panel border border-ide-border rounded select-all">
              {CONFIRM_TEXT}
            </p>
            <input
              type="text"
              value={confirmText}
              onChange={(e) => setConfirmText(e.target.value)}
              className="w-full px-3 py-2 bg-ide-panel border border-ide-border rounded-lg text-sm text-ide-text focus:outline-none focus:border-ide-accent"
              placeholder=""
              autoFocus
            />
          </div>

          <div className="flex justify-end gap-2 pt-2">
            <button
              onClick={() => setShowPrivacyDialog(false)}
              className="px-4 py-2 text-sm text-ide-muted hover:text-ide-text hover:bg-ide-hover rounded-lg transition-colors"
            >
              {t('settings.ai_embedding.privacy_warning.cancel_button')}
            </button>
            <button
              onClick={handleConfirmEnable}
              disabled={confirmText !== CONFIRM_TEXT}
              className="px-4 py-2 text-sm bg-ide-accent text-white rounded-lg hover:bg-ide-accent/80 transition-colors disabled:opacity-30 disabled:cursor-not-allowed"
            >
              {t('settings.ai_embedding.privacy_warning.confirm_button')}
            </button>
          </div>
        </div>
      </Dialog>

      {/* Reset token confirmation */}
      <ConfirmDialog
        isOpen={showResetConfirm}
        onCancel={() => setShowResetConfirm(false)}
        onConfirm={handleResetToken}
        title={t('settings.ai_embedding.token.reset_confirm_title')}
        message={t('settings.ai_embedding.token.reset_confirm_message')}
        confirmVariant="danger"
      />
    </div>
  );
}
