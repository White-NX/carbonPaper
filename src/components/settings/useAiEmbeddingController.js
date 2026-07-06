import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../lib/auth_api';
import { useTauriEventListener } from '../../hooks/useTauriEventListener';

export function useAiEmbeddingController({ t, agentSkillName, agentSkillRepo }) {
  const [enabled, setEnabled] = useState(() => localStorage.getItem('mcpEnabled') === 'true');
  const [port, setPort] = useState(() => {
    const saved = parseInt(localStorage.getItem('mcpPort'), 10);
    return saved > 0 ? saved : 23816;
  });
  const [running, setRunning] = useState(false);
  const [serviceState, setServiceState] = useState(() => (
    localStorage.getItem('mcpEnabled') === 'true' ? 'pending_auth' : 'disabled'
  ));
  const [statusError, setStatusError] = useState('');
  const hasCachedState = localStorage.getItem('mcpEnabled') !== null;
  const [loading, setLoading] = useState(!hasCachedState);
  const [actionLoading, setActionLoading] = useState(false);
  const [restoreLoading, setRestoreLoading] = useState(false);
  const [error, setError] = useState('');
  const restoreAttemptRef = useRef('');
  const [privacyAcknowledged, setPrivacyAcknowledged] = useState(false);
  const [showPrivacyDialog, setShowPrivacyDialog] = useState(false);
  const [confirmText, setConfirmText] = useState('');
  const [tokenCopied, setTokenCopied] = useState(false);
  const [agentPromptCopied, setAgentPromptCopied] = useState(false);
  const [showResetConfirm, setShowResetConfirm] = useState(false);
  const [filterEnabled, setFilterEnabled] = useState(true);
  const [filterCategories, setFilterCategories] = useState({
    cat_01: true, cat_02: true, cat_03: true, cat_04: true, cat_05: true,
  });
  const [filterMode, setFilterMode] = useState('reject');
  const [showAdvanced, setShowAdvanced] = useState(false);
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
      setServiceState(status.state || (status.enabled ? (status.running ? 'running' : 'pending_auth') : 'disabled'));
      setStatusError(status.error || '');
      setPrivacyAcknowledged(Boolean(status.privacy_acknowledged));

      localStorage.setItem('mcpEnabled', status.enabled ? 'true' : 'false');
      if (status.port) localStorage.setItem('mcpPort', String(status.port));

      try {
        const filterConfig = await withAuth(() => invoke('mcp_get_sensitive_filter_config'));
        setFilterEnabled(filterConfig.enabled);
        setFilterCategories(filterConfig.categories);
        if (filterConfig.mode) setFilterMode(filterConfig.mode);
        if (filterConfig.presidio_enabled !== undefined) setPiiEnabled(filterConfig.presidio_enabled);
        if (filterConfig.presidio_entities && filterConfig.presidio_entities.length > 0) {
          const entityMap = {};
          for (const e of filterConfig.presidio_entities) entityMap[e] = true;
          setPiiEntities((prev) => {
            const merged = { ...prev };
            for (const key of Object.keys(merged)) merged[key] = !!entityMap[key];
            return merged;
          });
        }
      } catch (e) {
        console.error('Failed to load filter config:', e);
      }

      try {
        const models = await invoke('check_spacy_models');
        setSpacyModels(models);
      } catch (e) {
        console.error('Failed to check spaCy models:', e);
      }
    } catch (e) {
      console.error('Failed to load MCP status:', e);
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

  useTauriEventListener('mcp-status-changed', () => {
    loadStatus();
  }, [loadStatus]);

  useTauriEventListener('spacy-model-status', (event) => {
    const { model, status } = event.payload;
    if (status === 'installing') {
      setDownloadingModel(model);
    } else if (status === 'installed') {
      setSpacyModels((prev) => ({ ...prev, [model]: { installed: true } }));
      setDownloadingModel((prev) => prev === model ? null : prev);
    } else if (status === 'failed') {
      setDownloadingModel((prev) => prev === model ? null : prev);
    }
  });

  const startMcpService = useCallback(async ({ auto = false } = {}) => {
    const setBusy = auto ? setRestoreLoading : setActionLoading;
    setBusy(true);
    if (!auto) setError('');
    try {
      const result = await withAuth(
        () => invoke('mcp_set_enabled', { enabled: true }),
        { autoPrompt: !auto },
      );
      setEnabled(true);
      setRunning(true);
      setServiceState('running');
      setStatusError('');
      localStorage.setItem('mcpEnabled', 'true');
      if (result.port) {
        setPort(result.port);
        localStorage.setItem('mcpPort', String(result.port));
      }
      setTokenCopied(false);
      return true;
    } catch (e) {
      const message = String(e);
      if (!auto) setError(message);
      setRunning(false);
      if (message.includes('AUTH_REQUIRED')) {
        setServiceState('pending_auth');
      } else {
        setServiceState('error');
        setStatusError(message);
      }
      return false;
    } finally {
      setBusy(false);
    }
  }, []);

  const handleToggle = async () => {
    if (!enabled) {
      if (privacyAcknowledged) {
        await startMcpService({ auto: false });
      } else {
        setShowPrivacyDialog(true);
        setConfirmText('');
      }
    } else {
      setActionLoading(true);
      setError('');
      try {
        await withAuth(() => invoke('mcp_set_enabled', { enabled: false }), { autoPrompt: true });
        setEnabled(false);
        setRunning(false);
        setServiceState('disabled');
        setStatusError('');
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
      await invoke('mcp_ack_privacy_warning');
      setPrivacyAcknowledged(true);
    } catch (e) {
      setError(String(e));
      setActionLoading(false);
      return;
    }
    setActionLoading(false);
    await startMcpService({ auto: false });
  };

  const handleResetToken = async () => {
    setShowResetConfirm(false);
    setActionLoading(true);
    setError('');
    try {
      const result = await withAuth(() => invoke('mcp_reset_token'), { autoPrompt: true });
      setTokenCopied(Boolean(result?.copied_to_clipboard));
    } catch (e) {
      setError(String(e));
    } finally {
      setActionLoading(false);
    }
  };

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
      await withAuth(() => invoke('mcp_set_sensitive_filter_config', {
        config: {
          enabled: newEnabled,
          categories: newCategories,
          mode: filterMode,
          presidio_enabled: piiEnabled,
          presidio_entities: Object.keys(piiEntities).filter((k) => piiEntities[k]),
        },
      }), { autoPrompt: true });
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
      await withAuth(() => invoke('mcp_set_sensitive_filter_config', {
        config: {
          enabled: filterEnabled,
          categories: newCategories,
          mode: filterMode,
          presidio_enabled: piiEnabled,
          presidio_entities: Object.keys(piiEntities).filter((k) => piiEntities[k]),
        },
      }), { autoPrompt: true });
    } catch (e) {
      setFilterCategories(filterCategories);
      console.error('Failed to save filter config:', e);
    }
  };

  const handleFilterModeChange = async (newMode) => {
    const prevMode = filterMode;
    setFilterMode(newMode);
    try {
      await withAuth(() => invoke('mcp_set_sensitive_filter_config', {
        config: {
          enabled: filterEnabled,
          categories: filterCategories,
          mode: newMode,
          presidio_enabled: piiEnabled,
          presidio_entities: Object.keys(piiEntities).filter((k) => piiEntities[k]),
        },
      }), { autoPrompt: true });
    } catch (e) {
      setFilterMode(prevMode);
      console.error('Failed to save filter config:', e);
    }
  };

  const savePiiConfig = async (newEnabled, newEntities) => {
    try {
      const entityList = Object.keys(newEntities).filter((k) => newEntities[k]);
      await withAuth(() => invoke('mcp_set_sensitive_filter_config', {
        config: {
          enabled: filterEnabled,
          categories: filterCategories,
          mode: filterMode,
          presidio_enabled: newEnabled,
          presidio_entities: entityList,
        },
      }), { autoPrompt: true });
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

  const handleCopyCurrentToken = async () => {
    try {
      await withAuth(() => invoke('mcp_copy_token_to_clipboard'), { autoPrompt: true });
      setTokenCopied(true);
    } catch (e) {
      setError(String(e));
    }
  };

  const handleCopyAgentSetupPrompt = async () => {
    const endpoint = `http://localhost:${port}/mcp`;
    const prompt = t('settings.ai_embedding.agent_setup.prompt', {
      skillName: agentSkillName,
      repo: agentSkillRepo,
      endpoint,
    });
    try {
      await navigator.clipboard.writeText(prompt);
      setAgentPromptCopied(true);
    } catch (e) {
      setError(String(e));
    }
  };

  const normalizedServiceState = enabled
    ? (running ? 'running' : serviceState || 'pending_auth')
    : 'disabled';
  const shouldShowStartButton = enabled && normalizedServiceState !== 'running';
  const statusBadge = {
    running: { label: 'RUNNING', className: 'text-green-500' },
    pending_auth: { label: 'WAITING', className: 'text-amber-400' },
    error: { label: 'ERROR', className: 'text-red-500' },
    stopped: { label: 'STOPPED', className: 'text-red-500' },
  }[normalizedServiceState] || { label: 'STOPPED', className: 'text-red-500' };
  const statusMessage = (() => {
    if (restoreLoading) return t('settings.ai_embedding.status.starting');
    if (!enabled) return t('settings.ai_embedding.status.stopped');
    if (normalizedServiceState === 'running') {
      return `${t('settings.ai_embedding.status.port_label')}: ${port}`;
    }
    if (normalizedServiceState === 'pending_auth') {
      return t('settings.ai_embedding.status.pending_auth');
    }
    if (normalizedServiceState === 'error') {
      return statusError || t('settings.ai_embedding.status.error');
    }
    return t('settings.ai_embedding.status.stopped');
  })();

  useEffect(() => {
    if (!enabled || running) {
      restoreAttemptRef.current = '';
      return;
    }
    if (normalizedServiceState !== 'stopped' || actionLoading || restoreLoading) return;

    const attemptKey = String(port || 23816);
    if (restoreAttemptRef.current === attemptKey) return;

    restoreAttemptRef.current = attemptKey;
    startMcpService({ auto: true });
  }, [enabled, running, normalizedServiceState, actionLoading, restoreLoading, port, startMcpService]);

  useEffect(() => {
    if (!enabled || running) return undefined;

    const timer = window.setInterval(() => {
      loadStatus();
    }, 5000);

    return () => window.clearInterval(timer);
  }, [enabled, running, loadStatus]);

  return {
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
  };
}
