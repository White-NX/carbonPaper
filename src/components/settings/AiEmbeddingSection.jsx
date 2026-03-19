import React, { useState, useEffect, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, Copy, Check, RefreshCw, HelpCircle, ChevronDown, ChevronUp, Download, Loader2, Paperclip } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Dialog } from '../Dialog';
import { ConfirmDialog } from '../ConfirmDialog';

export default function AiEmbeddingSection() {
  const { t } = useTranslation();
  const [enabled, setEnabled] = useState(() => localStorage.getItem('mcpEnabled') === 'true');
  const [port, setPort] = useState(() => {
    const saved = parseInt(localStorage.getItem('mcpPort'), 10);
    return saved > 0 ? saved : 23816;
  });
  const [running, setRunning] = useState(false);
  // Skip loading spinner if we have cached state from localStorage
  const hasCachedState = localStorage.getItem('mcpEnabled') !== null;
  const [loading, setLoading] = useState(!hasCachedState);
  const [actionLoading, setActionLoading] = useState(false);
  const [error, setError] = useState('');

  // Privacy warning dialog
  const [showPrivacyDialog, setShowPrivacyDialog] = useState(false);
  const [confirmText, setConfirmText] = useState('');

  // Token display (one-time)
  const [token, setToken] = useState(null);
  const [tokenCopied, setTokenCopied] = useState(false);

  // Token reset confirmation
  const [showResetConfirm, setShowResetConfirm] = useState(false);

  // Content filter
  const [filterEnabled, setFilterEnabled] = useState(true);
  const [filterCategories, setFilterCategories] = useState({
    cat_01: true, cat_02: true, cat_03: true, cat_04: true, cat_05: true
  });
  const [filterMode, setFilterMode] = useState('reject');
  const [showAdvanced, setShowAdvanced] = useState(false);

  // PII Detection
  const [piiEnabled, setPiiEnabled] = useState(true);
  const [piiEntities, setPiiEntities] = useState({
    PHONE_NUMBER: true, CN_ID_CARD: true,
    EMAIL_ADDRESS: true, CN_BANK_CARD: true, ADDRESS: true,
  });
  const [spacyModels, setSpacyModels] = useState({
    zh_core_web_sm: { installed: false },
    en_core_web_sm: { installed: false },
  });
  const [downloadingModel, setDownloadingModel] = useState(null);
  const [recheckLoading, setRecheckLoading] = useState(false);
  const [showPiiAdvanced, setShowPiiAdvanced] = useState(false);

  const CONFIRM_TEXT = t('settings.ai_embedding.privacy_warning.confirm_text');

  const loadStatus = useCallback(async (retryCount = 0) => {
    try {
      const status = await invoke('mcp_get_status');
      setEnabled(status.enabled);
      setPort(status.port);
      setRunning(status.running);

      // Sync to localStorage for instant state on next mount
      localStorage.setItem('mcpEnabled', status.enabled ? 'true' : 'false');
      if (status.port) localStorage.setItem('mcpPort', String(status.port));

      // Load filter config
      try {
        const filterConfig = await invoke('mcp_get_sensitive_filter_config');
        setFilterEnabled(filterConfig.enabled);
        setFilterCategories(filterConfig.categories);
        if (filterConfig.mode) setFilterMode(filterConfig.mode);
        if (filterConfig.presidio_enabled !== undefined) setPiiEnabled(filterConfig.presidio_enabled);
        if (filterConfig.presidio_entities && filterConfig.presidio_entities.length > 0) {
          const entityMap = {};
          for (const e of filterConfig.presidio_entities) entityMap[e] = true;
          setPiiEntities(prev => {
            const merged = { ...prev };
            for (const key of Object.keys(merged)) merged[key] = !!entityMap[key];
            return merged;
          });
        }
      } catch (e) {
        console.error('Failed to load filter config:', e);
      }

      // Load spaCy model status
      try {
        const models = await invoke('check_spacy_models');
        setSpacyModels(models);
      } catch (e) {
        console.error('Failed to check spaCy models:', e);
      }
    } catch (e) {
      console.error('Failed to load MCP status:', e);
      // Retry once after a short delay (IPC bridge may not be ready after refresh)
      if (retryCount < 1) {
        setTimeout(() => loadStatus(retryCount + 1), 500);
        return;
      }
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadStatus();
  }, [loadStatus]);

  // Listen for background auto-install events
  useEffect(() => {
    const unlisten = listen('spacy-model-status', (event) => {
      const { model, status } = event.payload;
      if (status === 'installing') {
        setDownloadingModel(model);
      } else if (status === 'installed') {
        setSpacyModels(prev => ({ ...prev, [model]: { installed: true } }));
        setDownloadingModel(prev => prev === model ? null : prev);
      } else if (status === 'failed') {
        setDownloadingModel(prev => prev === model ? null : prev);
      }
    });
    return () => { unlisten.then(fn => fn()); };
  }, []);

  const handleToggle = async () => {
    if (!enabled) {
      setShowPrivacyDialog(true);
      setConfirmText('');
    } else {
      setActionLoading(true);
      setError('');
      try {
        await invoke('mcp_set_enabled', { enabled: false });
        setEnabled(false);
        setRunning(false);
        localStorage.setItem('mcpEnabled', 'false');
      } catch (e) {
        setError(String(e));
      } finally {
        setActionLoading(false);
      }
    }
  };

  const handleConfirmEnable = async () => {
    setShowPrivacyDialog(false);
    setActionLoading(true);
    setError('');
    try {
      const result = await invoke('mcp_set_enabled', { enabled: true });
      setEnabled(true);
      setRunning(true);
      localStorage.setItem('mcpEnabled', 'true');
      if (result.port) {
        setPort(result.port);
        localStorage.setItem('mcpPort', String(result.port));
      }
      if (result.token) {
        setToken(result.token);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setActionLoading(false);
    }
  };

  const handleResetToken = async () => {
    setShowResetConfirm(false);
    setActionLoading(true);
    setError('');
    try {
      const result = await invoke('mcp_reset_token');
      if (result.token) {
        setToken(result.token);
        setTokenCopied(false);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setActionLoading(false);
    }
  };

  // Derive filter level from current state
  const filterLevel = (() => {
    if (!filterEnabled) return 'off';
    const { cat_01, cat_02, cat_03, cat_04, cat_05 } = filterCategories;
    if (cat_01 && cat_02 && cat_03 && cat_04 && cat_05) return 'standard';
    if (cat_02 && cat_05 && !cat_01 && !cat_03 && !cat_04) return 'minimal';
    return 'custom';
  })();

  const handleLevelChange = async (level) => {
    let newEnabled = filterEnabled;
    let newCategories = { ...filterCategories };
    if (level === 'standard') {
      newEnabled = true;
      newCategories = { cat_01: true, cat_02: true, cat_03: true, cat_04: true, cat_05: true };
    } else if (level === 'minimal') {
      newEnabled = true;
      newCategories = { cat_01: false, cat_02: true, cat_03: false, cat_04: false, cat_05: true };
    } else if (level === 'off') {
      newEnabled = false;
    }
    setFilterEnabled(newEnabled);
    setFilterCategories(newCategories);
    try {
      await invoke('mcp_set_sensitive_filter_config', {
        config: { enabled: newEnabled, categories: newCategories, mode: filterMode,
          presidio_enabled: piiEnabled, presidio_entities: Object.keys(piiEntities).filter(k => piiEntities[k]) }
      });
    } catch (e) {
      setFilterEnabled(filterEnabled);
      setFilterCategories(filterCategories);
      console.error('Failed to save filter config:', e);
    }
  };

  const handleCategoryToggle = async (category) => {
    const newCategories = { ...filterCategories, [category]: !filterCategories[category] };
    setFilterCategories(newCategories);
    try {
      await invoke('mcp_set_sensitive_filter_config', {
        config: { enabled: filterEnabled, categories: newCategories, mode: filterMode,
          presidio_enabled: piiEnabled, presidio_entities: Object.keys(piiEntities).filter(k => piiEntities[k]) }
      });
    } catch (e) {
      setFilterCategories(filterCategories);
      console.error('Failed to save filter config:', e);
    }
  };

  const handleFilterModeChange = async (newMode) => {
    const prevMode = filterMode;
    setFilterMode(newMode);
    try {
      await invoke('mcp_set_sensitive_filter_config', {
        config: { enabled: filterEnabled, categories: filterCategories, mode: newMode,
          presidio_enabled: piiEnabled, presidio_entities: Object.keys(piiEntities).filter(k => piiEntities[k]) }
      });
    } catch (e) {
      setFilterMode(prevMode);
      console.error('Failed to save filter config:', e);
    }
  };

  const savePiiConfig = async (newEnabled, newEntities) => {
    try {
      const entityList = Object.keys(newEntities).filter(k => newEntities[k]);
      await invoke('mcp_set_sensitive_filter_config', {
        config: {
          enabled: filterEnabled, categories: filterCategories, mode: filterMode,
          presidio_enabled: newEnabled,
          presidio_entities: entityList,
        }
      });
    } catch (e) {
      console.error('Failed to save PII config:', e);
    }
  };

  const handlePiiToggle = async () => {
    const newVal = !piiEnabled;
    setPiiEnabled(newVal);
    await savePiiConfig(newVal, piiEntities);
  };

  const handlePiiEntityToggle = async (entityType) => {
    const newEntities = { ...piiEntities, [entityType]: !piiEntities[entityType] };
    setPiiEntities(newEntities);
    await savePiiConfig(piiEnabled, newEntities);
  };

  const handleDownloadModel = async (modelName) => {
    setDownloadingModel(modelName);
    try {
      await invoke('install_spacy_model', { modelName });
      // Refresh model status
      const models = await invoke('check_spacy_models');
      setSpacyModels(models);
    } catch (e) {
      setError(String(e));
    } finally {
      setDownloadingModel(null);
    }
  };

  const handleForceRecheck = async () => {
    setRecheckLoading(true);
    try {
      const models = await invoke('force_recheck_spacy_models');
      setSpacyModels(models);
    } catch (e) {
      setError(String(e));
    } finally {
      setRecheckLoading(false);
    }
  };

  const handleCopyToken = async () => {
    if (!token) return;
    try {
      await navigator.clipboard.writeText(token);
      setTokenCopied(true);
      setTimeout(() => setTokenCopied(false), 2000);
    } catch {
      const ta = document.createElement('textarea');
      ta.value = token;
      document.body.appendChild(ta);
      ta.select();
      document.execCommand('copy');
      document.body.removeChild(ta);
      setTokenCopied(true);
      setTimeout(() => setTokenCopied(false), 2000);
    }
  };

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
        <div className="relative group">
          <HelpCircle className="w-3.5 h-3.5 text-ide-muted cursor-help" />
          <div className="absolute left-1/2 -translate-x-1/3 top-full mt-2 w-60 px-3 py-2 bg-ide-panel border border-ide-border rounded-lg shadow-lg text-xs text-ide-muted opacity-0 pointer-events-none group-hover:opacity-100 group-hover:pointer-events-auto transition-opacity z-50">
            {t('settings.ai_embedding.mcp_description')}
          </div>
        </div>
      </div>

      {/* Options card — single card with dividers, matching MonitorServiceSection */}
      <div className="p-4 bg-ide-bg border border-ide-border rounded-xl text-sm text-ide-muted space-y-3">
        {/* Row 1: Enable toggle + status */}
        <div className="flex items-center justify-between gap-4">
          <div>
            <label className="block mb-1 font-semibold text-ide-text">
              {t('settings.ai_embedding.enable_label')}{' '}
              {enabled && (
                <span className={running ? 'text-green-500' : 'text-red-500'}>
                  {running ? 'RUNNING' : 'STOPPED'}
                </span>
              )}
            </label>
            <p className="text-xs text-ide-muted">
              {enabled
                ? `${t('settings.ai_embedding.status.port_label')}: ${port}`
                : t('settings.ai_embedding.status.stopped')}
            </p>
          </div>
          <button
            onClick={handleToggle}
            disabled={actionLoading}
            className={`relative w-10 h-5 rounded-full transition-colors shrink-0 ${enabled ? 'bg-ide-accent' : 'bg-ide-border'
              } disabled:opacity-50`}
          >
            <div
              className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform ${enabled ? 'translate-x-5' : 'translate-x-0.5'
                }`}
            />
          </button>
        </div>

        {error && (
          <div className="p-3 bg-red-500/10 border border-red-500/30 rounded-lg">
            <p className="text-xs text-red-400">{error}</p>
          </div>
        )}

        {/* Row 2: Token management (when enabled, token not being shown) */}
        {enabled && !token && (
          <>
            <div className="w-full h-px bg-ide-border/50" />
            <div className="flex items-center justify-between gap-4">
              <div className="flex-1">
                <label className="block mb-1 font-semibold text-ide-text">{t('settings.ai_embedding.token.title')}</label>
                <p className="text-xs text-ide-muted">{t('settings.ai_embedding.token.description')}</p>
              </div>
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

        {/* Row 3: Connection info (when enabled, running, token not being shown) */}
        {enabled && !token && running && (
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

        {/* Row 4: Privacy protection level (when enabled, token not being shown) */}
        {enabled && !token && (
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

        {/* Row 5: Filter mode (when enabled, not off, token not being shown) */}
        {enabled && !token && filterEnabled && (
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

        {/* Row 6: PII Detection (when enabled, token not being shown) */}
        {enabled && !token && (
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
                <button
                  onClick={handlePiiToggle}
                  className={`relative w-10 h-5 rounded-full transition-colors shrink-0 ${piiEnabled ? 'bg-ide-accent' : 'bg-ide-border'}`}
                >
                  <div
                    className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform ${piiEnabled ? 'translate-x-5' : 'translate-x-0.5'}`}
                  />
                </button>
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

      {/* Token one-time display — separate alert card (temporary state) */}
      {token && (
        <div className="p-4 bg-ide-bg border border-ide-warning-border rounded-xl space-y-3">
          <div className="flex items-start gap-2">
            <AlertTriangle className="w-4 h-4 text-ide-warning shrink-0 mt-0.5" />
            <p className="text-xs text-ide-warning leading-relaxed">
              {t('settings.ai_embedding.token.show_once_warning')}
            </p>
          </div>
          <div className="flex items-center gap-2">
            <code className="flex-1 px-3 py-2 bg-ide-panel border border-ide-border rounded-lg text-xs text-ide-text font-mono break-all select-all">
              {token}
            </code>
            <button
              onClick={handleCopyToken}
              className="shrink-0 px-3 py-2 bg-ide-panel border border-ide-border rounded-lg text-xs text-ide-text hover:bg-ide-hover transition-colors flex items-center gap-1.5"
            >
              {tokenCopied ? (
                <><Check className="w-3.5 h-3.5 text-green-400" />{t('settings.ai_embedding.token.copied')}</>
              ) : (
                <><Copy className="w-3.5 h-3.5" />{t('settings.ai_embedding.token.copy')}</>
              )}
            </button>
          </div>
          <button
            onClick={() => { setToken(null); setTokenCopied(false); }}
            className="w-full py-1.5 text-xs text-ide-muted hover:text-ide-text hover:bg-ide-hover rounded-lg transition-colors"
          >
            {t('settings.ai_embedding.privacy_warning.cancel_button')}
          </button>
        </div>
      )}

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
