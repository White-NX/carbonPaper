import { invoke } from '@tauri-apps/api/core';
import { withAuth } from './auth_api';

export const getSecurityPolicy = () => invoke('security_get_policy');
export const acknowledgeSecurityIntro = () => invoke('security_acknowledge_intro');
export const setSecurityPolicy = (policy) => withAuth(() => invoke('security_set_policy', {
  level: policy.level,
  sessionTimeout: policy.session_timeout,
  unattendedEnabled: policy.unattended_enabled,
}), { promptMode: 'action' });

export const getAgentAccessStatus = () => invoke('agent_access_get_status');
export const resumeAgentAccess = () => withAuth(() => invoke('agent_access_resume'), { promptMode: 'action' });
export const issueAgentCredential = ({ label, expiresAtMs = null, replaceGrantId = null }) => withAuth(
  () => invoke('agent_access_issue_credential', { label, expiresAtMs, replaceGrantId }), { promptMode: 'action' },
);
export const revokeAgentCredential = (grantId) => withAuth(
  () => invoke('agent_access_revoke_credential', { grantId }), { promptMode: 'action' },
);
