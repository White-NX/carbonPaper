import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { getSecurityPolicy, setSecurityPolicy } from '../../../lib/agent_access_api';
import { notifySettingsChanged, openSettingsWindow } from '../../../lib/settings_api';
import { useTauriEventListener } from '../../../hooks/useTauriEventListener';
import { usePolling } from '../../../hooks/usePolling';
import { useSettingsActive, useSettingsActivity } from '../SettingsActivityContext';
import { needsUnattendedWarning, policyForLevel, securityErrorMessage } from './securityLevels';

export function useSecurityLevel({ enabled = true, onSessionTimeoutChange } = {}) {
  const { t } = useTranslation();
  const settingsActive = useSettingsActive();
  const active = settingsActive && enabled;
  const [policy, setPolicy] = useState(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [loadError, setLoadError] = useState('');
  const [actionError, setActionError] = useState('');
  const [pending, setPending] = useState(null);
  const sequence = useRef(0);
  const read = useRef(null);
  const queued = useRef(false);
  const activeRef = useRef(active);
  activeRef.current = active;
  const busy = useRef(false);
  const mounted = useRef(true);
  const onTimeout = useRef(onSessionTimeoutChange);
  onTimeout.current = onSessionTimeoutChange;
  useSettingsActivity('security-policy', { busy: saving });

  const refresh = useCallback(() => {
    if (read.current) return read.current;
    const token = ++sequence.current;
    const work = getSecurityPolicy().then((next) => {
      if (mounted.current && token === sequence.current) {
        setPolicy(next); setLoading(false); setLoadError('');
      }
      return next;
    }).catch((cause) => {
      if (mounted.current && token === sequence.current) {
        setLoadError(t('settings.security.feedback.readFailed')); setLoading(false);
      }
      return null;
    }).finally(() => {
      if (read.current === work) read.current = null;
      if (queued.current && mounted.current && activeRef.current) {
        queued.current = false; refresh();
      }
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
    mounted.current = true;
    return () => { mounted.current = false; sequence.current += 1; };
  }, []);
  useEffect(() => {
    if (active) invalidate();
    return () => { sequence.current += 1; queued.current = false; };
  }, [active, invalidate]);
  useTauriEventListener('agent-access-changed', invalidate, [invalidate], active);
  useTauriEventListener('settings-preferences-changed', ({ payload }) => {
    if (payload?.some((key) => ['session', 'security'].includes(key))) invalidate();
  }, [invalidate], active);
  useEffect(() => {
    if (!active) return undefined;
    const changed = ({ detail }) => { if (detail?.some((key) => ['session', 'security'].includes(key))) invalidate(); };
    window.addEventListener('settings-preferences-changed', changed);
    return () => window.removeEventListener('settings-preferences-changed', changed);
  }, [active, invalidate]);
  usePolling(refresh, { intervalMs: 2500, enabled: active && policy?.unattended_enabled === true && policy?.phase !== 'ready' });

  const apply = useCallback(async (next) => {
    if (busy.current) return false;
    busy.current = true; sequence.current += 1;
    if (read.current) queued.current = true;
    setSaving(true); setActionError('');
    try {
      const saved = await setSecurityPolicy(next);
      sequence.current += 1;
      if (mounted.current) {
        setPolicy(saved); onTimeout.current?.(saved.session_timeout);
        try { localStorage.setItem('sessionTimeout', String(saved.session_timeout)); } catch { }
      }
      await notifySettingsChanged(['session', 'security']);
      return true;
    } catch (cause) {
      if (mounted.current) setActionError(securityErrorMessage(cause, t));
      await invalidate();
      return false;
    } finally { busy.current = false; if (mounted.current) setSaving(false); }
  }, [invalidate, t]);
  const requestChange = (next) => {
    if (!policy || busy.current || pending) return;
    if (needsUnattendedWarning(policy, next)) setPending(next);
    else apply(next);
  };
  const confirm = async () => { if (pending) { await apply(pending); setPending(null); } };
  return { policy, loading, saving, error: actionError || loadError, pending, refresh, apply, requestChange, confirm,
    openComponents: () => openSettingsWindow('privacy', 'protected-components').catch((cause) => setActionError(securityErrorMessage(cause, t))),
    cancel: () => { if (!busy.current) setPending(null); }, selectLevel: (level) => requestChange(policyForLevel(level, policy)) };
}
