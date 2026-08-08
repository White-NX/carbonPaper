import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../../lib/auth_api';
import { useTauriEventListener } from '../../hooks/useTauriEventListener';
import {
  AGENT_SETUP_VARIANTS,
  AGENT_SKILL_NAME,
  AGENT_SKILL_REPO,
  DEFAULT_AGENT_SETUP_VARIANT,
} from './agent-access/agentAccessConstants';
import { useSensitiveFilterSettings } from './agent-access/useSensitiveFilterSettings';

function isCurrentMcpSmokeReport(report, status) {
  if (
    !report
    || report.runtime_generation == null
    || report.runtime_generation !== status.runtime_generation
  ) {
    return false;
  }
  if (!report.ok) return true;
  return Boolean(
    status.running
    && status.port_consistent === true
    && status.active_port === status.port,
  );
}

export function useAiEmbeddingController({ t }) {
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
  const [diagnosticsCopied, setDiagnosticsCopied] = useState(false);
  const [agentVariant, setAgentVariant] = useState(() => {
    const saved = localStorage.getItem('mcpAgentSetupVariant');
    return AGENT_SETUP_VARIANTS.includes(saved) ? saved : DEFAULT_AGENT_SETUP_VARIANT;
  });
  const [agentSkill, setAgentSkill] = useState({
    id: AGENT_SKILL_NAME,
    source_repository: AGENT_SKILL_REPO,
    tool_schema_version: null,
  });
  const [smokeTestLoading, setSmokeTestLoading] = useState(false);
  const [smokeTestReport, setSmokeTestReport] = useState(null);
  const [showResetConfirm, setShowResetConfirm] = useState(false);
  const smokeTestRequestRef = useRef(0);
  const statusRequestRef = useRef(0);
  const serviceFingerprintRef = useRef(null);
  const mcpOperationRef = useRef(null);

  const CONFIRM_TEXT = t('settings.ai_embedding.privacy_warning.confirm_text');
  const sensitiveFilter = useSensitiveFilterSettings({ t, onError: setError });
  const { loadFilterConfig, loadSpacyModels } = sensitiveFilter;

  const invalidateSmokeTestReport = useCallback(() => {
    smokeTestRequestRef.current += 1;
    setSmokeTestReport(null);
  }, []);

  const beginMcpOperation = useCallback((name) => {
    if (mcpOperationRef.current) return null;
    const operation = Symbol(name);
    mcpOperationRef.current = operation;
    return operation;
  }, []);

  const finishMcpOperation = useCallback((operation) => {
    if (mcpOperationRef.current === operation) {
      mcpOperationRef.current = null;
    }
  }, []);

  const loadStatus = useCallback(async (retryCount = 0) => {
    const requestId = statusRequestRef.current + 1;
    statusRequestRef.current = requestId;
    try {
      const status = await invoke('mcp_get_status');
      if (requestId !== statusRequestRef.current) return;
      const serviceFingerprint = JSON.stringify([
        Boolean(status.enabled),
        Boolean(status.running),
        status.state || '',
        status.port || null,
        status.active_port || null,
        status.port_consistent !== false,
        status.runtime_generation ?? null,
      ]);
      if (
        serviceFingerprintRef.current !== null
        && serviceFingerprintRef.current !== serviceFingerprint
      ) {
        invalidateSmokeTestReport();
      }
      serviceFingerprintRef.current = serviceFingerprint;

      setEnabled(status.enabled);
      setPort(status.port);
      setRunning(Boolean(status.running && status.port_consistent !== false));
      setServiceState(status.state || (status.enabled ? (status.running ? 'running' : 'pending_auth') : 'disabled'));
      setStatusError(status.error || '');
      setPrivacyAcknowledged(Boolean(status.privacy_acknowledged));
      if (status.skill) {
        setAgentSkill((current) => ({ ...current, ...status.skill }));
      }

      localStorage.setItem('mcpEnabled', status.enabled ? 'true' : 'false');
      if (status.port) localStorage.setItem('mcpPort', String(status.port));

      await loadFilterConfig();
      await loadSpacyModels();
    } catch (e) {
      if (requestId !== statusRequestRef.current) return;
      console.error('Failed to load MCP status:', e);
      if (retryCount < 1) {
        setTimeout(() => loadStatus(retryCount + 1), 500);
        return;
      }
      invalidateSmokeTestReport();
    } finally {
      if (requestId === statusRequestRef.current) setLoading(false);
    }
  }, [invalidateSmokeTestReport, loadFilterConfig, loadSpacyModels]);

  useEffect(() => {
    loadStatus();
  }, [loadStatus]);

  useTauriEventListener('mcp-status-changed', () => {
    invalidateSmokeTestReport();
    loadStatus();
  }, [invalidateSmokeTestReport, loadStatus]);

  const startMcpService = useCallback(async ({ auto = false, operation: existingOperation = null } = {}) => {
    const operation = existingOperation || beginMcpOperation(auto ? 'restore' : 'start');
    if (!operation) return null;
    const ownsOperation = existingOperation === null;
    const setBusy = auto ? setRestoreLoading : setActionLoading;
    invalidateSmokeTestReport();
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
      if (ownsOperation) finishMcpOperation(operation);
    }
  }, [beginMcpOperation, finishMcpOperation, invalidateSmokeTestReport]);

  const handleToggle = async () => {
    if (!enabled) {
      if (privacyAcknowledged) {
        await startMcpService({ auto: false });
      } else {
        setShowPrivacyDialog(true);
        setConfirmText('');
      }
    } else {
      const operation = beginMcpOperation('stop');
      if (!operation) return;
      invalidateSmokeTestReport();
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
        finishMcpOperation(operation);
      }
    }
  };

  const handleConfirmEnable = async () => {
    const operation = beginMcpOperation('enable');
    if (!operation) return;
    setShowPrivacyDialog(false);
    invalidateSmokeTestReport();
    setActionLoading(true);
    setError('');
    try {
      await invoke('mcp_ack_privacy_warning');
      setPrivacyAcknowledged(true);
      return await startMcpService({ auto: false, operation });
    } catch (e) {
      setError(String(e));
      return false;
    } finally {
      setActionLoading(false);
      finishMcpOperation(operation);
    }
  };

  const handleResetToken = async () => {
    const operation = beginMcpOperation('reset-token');
    if (!operation) return;
    setShowResetConfirm(false);
    invalidateSmokeTestReport();
    setActionLoading(true);
    setError('');
    try {
      const result = await withAuth(() => invoke('mcp_reset_token'), { autoPrompt: true });
      setTokenCopied(Boolean(result?.copied_to_clipboard));
    } catch (e) {
      setError(String(e));
    } finally {
      setActionLoading(false);
      finishMcpOperation(operation);
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
    const endpoint = `http://127.0.0.1:${port}/mcp`;
    const prompt = t(`settings.ai_embedding.agent_setup.variants.${agentVariant}.prompt`, {
      skillName: agentSkill.id,
      repo: agentSkill.source_repository,
      toolSchemaVersion: agentSkill.tool_schema_version ?? '?',
      endpoint,
    });
    try {
      await navigator.clipboard.writeText(prompt);
      setAgentPromptCopied(true);
    } catch (e) {
      setError(String(e));
    }
  };

  const handleAgentVariantChange = (variant) => {
    if (!AGENT_SETUP_VARIANTS.includes(variant)) return;
    setAgentVariant(variant);
    setAgentPromptCopied(false);
    localStorage.setItem('mcpAgentSetupVariant', variant);
  };

  const handleRunMcpSmokeTest = async () => {
    const operation = beginMcpOperation('smoke-test');
    if (!operation) return null;
    const requestId = smokeTestRequestRef.current + 1;
    smokeTestRequestRef.current = requestId;
    setSmokeTestLoading(true);
    setSmokeTestReport(null);
    setError('');
    try {
      const report = await withAuth(
        () => invoke('mcp_run_smoke_test'),
        { autoPrompt: true },
      );
      const currentStatus = await invoke('mcp_get_status');
      if (
        requestId === smokeTestRequestRef.current
        && isCurrentMcpSmokeReport(report, currentStatus)
      ) {
        setSmokeTestReport(report);
      }
      return report;
    } catch (e) {
      setError(String(e));
      return null;
    } finally {
      setSmokeTestLoading(false);
      finishMcpOperation(operation);
    }
  };

  const handleCopyAgentDiagnostics = async () => {
    try {
      const status = await invoke('mcp_get_status');
      const reportIsCurrent = isCurrentMcpSmokeReport(smokeTestReport, status);
      if (smokeTestReport && !reportIsCurrent) {
        invalidateSmokeTestReport();
      }
      const diagnostics = {
        carbonpaper_version: status.server_version,
        mcp_endpoint: `http://127.0.0.1:${status.port}/mcp`,
        mcp_state: status.state,
        skill: status.skill,
        capabilities: status.capabilities,
        smoke_test: reportIsCurrent ? smokeTestReport : null,
      };
      await navigator.clipboard.writeText(JSON.stringify(diagnostics, null, 2));
      setDiagnosticsCopied(true);
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
    if (
      normalizedServiceState !== 'stopped'
      || actionLoading
      || restoreLoading
      || smokeTestLoading
    ) return;

    const attemptKey = String(port || 23816);
    if (restoreAttemptRef.current === attemptKey) return;

    restoreAttemptRef.current = attemptKey;
    startMcpService({ auto: true }).then((started) => {
      if (started === null) {
        restoreAttemptRef.current = '';
      }
    });
  }, [
    enabled,
    running,
    normalizedServiceState,
    actionLoading,
    restoreLoading,
    smokeTestLoading,
    port,
    startMcpService,
  ]);

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
    diagnosticsCopied,
    agentVariant,
    agentSkill,
    smokeTestLoading,
    smokeTestReport,
    mcpOperationLoading: actionLoading || restoreLoading || smokeTestLoading,
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
    handleAgentVariantChange,
    handleRunMcpSmokeTest,
    handleCopyAgentDiagnostics,
    shouldShowStartButton,
    statusBadge,
    statusMessage,
  };
}
