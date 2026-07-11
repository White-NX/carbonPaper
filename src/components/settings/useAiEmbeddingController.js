import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../lib/auth_api';
import { useTauriEventListener } from '../../hooks/useTauriEventListener';
import { useSensitiveFilterSettings } from './agent-access/useSensitiveFilterSettings';

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

  const CONFIRM_TEXT = t('settings.ai_embedding.privacy_warning.confirm_text');
  const sensitiveFilter = useSensitiveFilterSettings({ t, onError: setError });
  const { loadFilterConfig, loadSpacyModels } = sensitiveFilter;

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

      await loadFilterConfig();
      await loadSpacyModels();
    } catch (e) {
      console.error('Failed to load MCP status:', e);
      if (retryCount < 1) {
        setTimeout(() => loadStatus(retryCount + 1), 500);
        return;
      }
    } finally {
      setLoading(false);
    }
  }, [loadFilterConfig, loadSpacyModels]);

  useEffect(() => {
    loadStatus();
  }, [loadStatus]);

  useTauriEventListener('mcp-status-changed', () => {
    loadStatus();
  }, [loadStatus]);

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
    ...sensitiveFilter,
    CONFIRM_TEXT,
    startMcpService,
    handleToggle,
    handleConfirmEnable,
    handleResetToken,
    handleCopyCurrentToken,
    handleCopyAgentSetupPrompt,
    shouldShowStartButton,
    statusBadge,
    statusMessage,
  };
}
