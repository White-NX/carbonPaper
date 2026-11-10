import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { getAgentAccessStatus, issueAgentCredential, resumeAgentAccess, revokeAgentCredential } from '../../../lib/agent_access_api';
import { useTauriEventListener } from '../../../hooks/useTauriEventListener';
import { usePolling } from '../../../hooks/usePolling';
import { useSettingsActive, useSettingsActivity } from '../SettingsActivityContext';
import { securityErrorMessage } from '../security/securityLevels';
import { openSettingsWindow } from '../../../lib/settings_api';
import { canIssueUnattendedCredential } from './agentAccessConstants';

export function useUnattendedCredentials() {
  const { t } = useTranslation();
  const active = useSettingsActive();
  const [status, setStatus] = useState(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [loadError, setLoadError] = useState('');
  const [actionError, setActionError] = useState('');
  const [notice, setNotice] = useState('');
  const [label, setLabel] = useState('');
  const [expiry, setExpiry] = useState('30d');
  const [replacement, setReplacement] = useState(null);
  const [revocation, setRevocation] = useState(null);
  const mounted = useRef(true);
  const read = useRef(null);
  const sequence = useRef(0);
  const queued = useRef(false);
  const activeRef = useRef(active);
  activeRef.current = active;
  const operation = useRef(false);
  useSettingsActivity('unattended-credentials', { busy, dirty: Boolean(label || replacement) });

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; sequence.current += 1; };
  }, []);
  const refresh = useCallback(() => {
    if (read.current) return read.current;
    const token = ++sequence.current;
    const work = getAgentAccessStatus().then((next) => {
      if (mounted.current && token === sequence.current) { setStatus(next); setLoadError(''); }
      return next;
    }).catch((cause) => {
      if (mounted.current && token === sequence.current) setLoadError(t('settings.security.credentials.readFailed'));
      return null;
    }).finally(() => {
      if (mounted.current && token === sequence.current) setLoading(false);
      if (read.current === work) read.current = null;
      if (queued.current && mounted.current && activeRef.current) { queued.current = false; refresh(); }
    });
    read.current = work;
    return work;
  }, [t]);
  const invalidate = useCallback(() => {
    sequence.current += 1;
    if (read.current) { queued.current = true; return read.current; }
    return refresh();
  }, [refresh]);
  useEffect(() => {
    if (active) invalidate();
    return () => { sequence.current += 1; queued.current = false; };
  }, [active, invalidate]);
  useTauriEventListener('agent-access-changed', invalidate, [invalidate], active);
  useTauriEventListener('auth-session-changed', ({ payload }) => {
    if (payload !== true) {
      setStatus((current) => current ? { ...current, grants: [] } : current);
      setRevocation(null);
    }
    invalidate();
  }, [invalidate], active);
  usePolling(refresh, { intervalMs: status?.phase === 'ready' ? 20000 : 4000, enabled: active && status?.enabled === true });

  const run = async (action, successKey) => {
    if (operation.current) return false;
    operation.current = true; sequence.current += 1;
    if (read.current) queued.current = true;
    setBusy(true); setActionError(''); setNotice('');
    try {
      await action();
      if (mounted.current) setNotice(t(successKey));
      return true;
    } catch (cause) {
      if (mounted.current) setActionError(securityErrorMessage(cause, t));
      return false;
    } finally {
      await invalidate();
      operation.current = false;
      if (mounted.current) setBusy(false);
    }
  };
  const issue = async () => {
    if (!label.trim() || !canIssueUnattendedCredential(status)) return false;
    const expiresAtMs = expiry === 'never' ? null : Date.now() + Number(expiry.slice(0, -1)) * 86400000;
    const succeeded = await run(async () => {
      const result = await issueAgentCredential({ label: label.trim(), expiresAtMs, replaceGrantId: replacement?.id ?? null });
      if (result?.copied_to_clipboard !== true) throw new Error('AGENT_CLIPBOARD_FAILED');
    }, replacement ? 'settings.security.credentials.replaced' : 'settings.security.credentials.created');
    if (succeeded && mounted.current) { setLabel(''); setReplacement(null); }
    return succeeded;
  };
  const replace = (grant) => {
    if (operation.current) return;
    setReplacement(grant); setLabel(grant.label); setExpiry('30d'); setNotice(''); setActionError('');
  };
  const cancelReplacement = () => {
    if (operation.current) return;
    setReplacement(null); setLabel('');
  };
  const revoke = async () => {
    if (!revocation) return;
    const grant = revocation;
    const succeeded = await run(() => revokeAgentCredential(grant.id), 'settings.security.credentials.revokedNotice');
    if (succeeded && replacement?.id === grant.id) cancelReplacement();
    if (mounted.current) setRevocation(null);
  };
  return { status, loading, busy, error: actionError || loadError, notice, label, setLabel, expiry, setExpiry,
    replacement, replace, cancelReplacement, revocation, requestRevoke: setRevocation,
    cancelRevoke: () => { if (!operation.current) setRevocation(null); }, revoke, issue, refresh,
    openSecurity: () => openSettingsWindow('privacy', 'security').catch((cause) => setActionError(securityErrorMessage(cause, t))),
    resume: () => run(resumeAgentAccess, 'settings.security.access.resumed') };
}
