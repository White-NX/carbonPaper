export const SECURITY_LEVELS = ['strict', 'relaxed', 'unattended', 'custom'];
const PRESETS = {
  strict: { session_timeout: 900, unattended_enabled: false },
  relaxed: { session_timeout: -1, unattended_enabled: false },
  unattended: { session_timeout: -1, unattended_enabled: true },
};

export function policyForLevel(level, policy) {
  if (!SECURITY_LEVELS.includes(level)) return policy;
  return { ...policy, ...PRESETS[level], level };
}

export function needsUnattendedWarning(current, next) {
  return (next.unattended_enabled && !current.unattended_enabled)
    || (next.level === 'unattended' && current.level !== 'unattended');
}

export function securityErrorMessage(error, t) {
  const message = typeof error === 'string' ? error : error?.message || String(error);
  if (/cancelled|canceled/i.test(message)) return t('settings.security.feedback.cancelled');
  if (/AUTH_NOT_AVAILABLE|WindowsHelloNotAvailable|Windows Hello is not available/i.test(message)) return t('settings.security.feedback.verificationUnavailable');
  if (message.includes('REPLACEMENT_REVOCATION_FAILED')) return t('settings.security.credentials.replacementIncomplete');
  if (message.includes('CLIPBOARD_FAILED')) return t('settings.security.credentials.clipboardFailed');
  if (/SYNC_INCOMPLETE|app_bound_task_busy/i.test(message)) return t('settings.security.access.preparing');
  if (message.includes('app_bound_limit_exceeded')) return t('settings.security.credentials.limitReached');
  if (/AGENT_ACCESS_DISABLED|app_bound_disabled|app_bound_dataset_mismatch|app_bound_task_retired/i.test(message)) return t('settings.security.access.phases.disabled');
  if (/AGENT_INVALID_GRANT|app_bound_invalid_request/i.test(message)) return t('settings.security.credentials.invalid');
  if (message.includes('PROTECTED_RUNTIME_REQUIRED') || message.includes('SERVICE_UNAVAILABLE')) return t('settings.security.access.requirements');
  return t('settings.feedback.saveFailed', { error: message });
}
