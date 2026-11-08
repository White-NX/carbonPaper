import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { acknowledgeSecurityIntro } from '../../../lib/agent_access_api';
import { useSettingsActivity } from '../SettingsActivityContext';
import { needsUnattendedWarning, securityErrorMessage } from './securityLevels';

export function useSecurityWizard({ open, security, onClose }) {
  const { t } = useTranslation();
  const [step, setStep] = useState(0);
  const [draft, setDraft] = useState(null);
  const [warning, setWarning] = useState(null);
  const [acknowledged, setAcknowledged] = useState(false);
  const [recording, setRecording] = useState(false);
  const [error, setError] = useState('');
  const initialized = useRef(false);
  const dismissing = useRef(false);
  useEffect(() => {
    if (!open) initialized.current = false;
    else if (!initialized.current && security.policy) {
      initialized.current = true;
      setDraft({ ...security.policy }); setStep(0); setWarning(null); setAcknowledged(false); setError('');
    }
  }, [open, security.policy]);
  useEffect(() => {
    if (!open || !security.policy) return;
    // Runtime progress can change while a user edits; keep their choices.
    setDraft((current) => current ? { ...current, available: security.policy.available } : current);
    setWarning((current) => current ? { ...current, available: security.policy.available } : current);
  }, [open, security.policy?.available]);
  const dirty = Boolean(open && draft && security.policy && ['level', 'session_timeout', 'unattended_enabled'].some((key) => draft[key] !== security.policy[key]));
  const busy = security.saving || recording;
  useSettingsActivity('security-wizard', { busy, dirty });

  const change = (next) => {
    if (busy || !draft) return;
    if (needsUnattendedWarning(draft, next)) setWarning(next);
    else { setDraft(next); if (!next.unattended_enabled) setAcknowledged(false); }
  };
  const acceptRisk = () => { if (warning && !busy) { setDraft(warning); setWarning(null); setAcknowledged(true); } };
  const close = async () => {
    if (dismissing.current || security.saving) return;
    dismissing.current = true; setRecording(true); setError('');
    try { await acknowledgeSecurityIntro(); onClose?.(); }
    catch (cause) { setError(securityErrorMessage(cause, t)); }
    finally { dismissing.current = false; setRecording(false); }
  };
  const finish = async () => {
    if (busy || !draft) return;
    if (needsUnattendedWarning(security.policy, draft) && !acknowledged) { setWarning(draft); return; }
    if (await security.apply(draft)) await close();
  };
  return { step, draft, warning, busy, error: error || security.error, change, acceptRisk,
    cancelRisk: () => setWarning(null), next: () => setStep((value) => Math.min(2, value + 1)),
    back: () => setStep((value) => Math.max(0, value - 1)), close, finish };
}
