import { Channel, invoke } from '@tauri-apps/api/core';
import { withAuth } from './auth_api';

/**
 * Model endpoints used by AI search.
 *
 * Settings can be read while locked; changes and connection tests need an
 * unlocked session because they read or write the encrypted API key.
 */
export function getAiSettings() {
  return invoke('ai_get_settings');
}

/** `apiKey`: `undefined` keeps the stored key, `''` clears it. */
export function saveAiProvider({ id, name, kind, baseUrl, model, apiKey }) {
  return withAuth(() => invoke('ai_save_provider', {
    provider: { id: id || null, name, kind, base_url: baseUrl, model, api_key: apiKey ?? null },
  }), { autoPrompt: true });
}

export function deleteAiProvider(id) {
  return withAuth(() => invoke('ai_delete_provider', { id }), { autoPrompt: true });
}

export function setDefaultAiProvider(id) {
  return withAuth(() => invoke('ai_set_default_provider', { id }), { autoPrompt: true });
}

export function testAiProvider({ id, name, kind, baseUrl, model, apiKey }) {
  return withAuth(() => invoke('ai_test_provider', {
    provider: { id: id || null, name, kind, base_url: baseUrl, model, api_key: apiKey ?? null },
  }), { autoPrompt: true });
}

export function grantAiRemoteConsent() {
  return withAuth(() => invoke('ai_grant_remote_consent'), { autoPrompt: true });
}

/**
 * Runs one AI search. `onEvent` receives the agent's progress events; the
 * promise resolves with the final outcome.
 */
export function runAiSearch({ requestId, question, providerId, onEvent }) {
  return withAuth(() => {
    const channel = new Channel();
    channel.onmessage = onEvent;
    return invoke('ai_search', { requestId, question, providerId: providerId || null, onEvent: channel });
  }, { autoPrompt: true });
}

export function cancelAiSearch(requestId) {
  return invoke('ai_search_cancel', { requestId });
}

/** Maps a backend error string such as `AI_UNAUTHORIZED: ...` to a message key. */
export function aiErrorKey(error) {
  const text = String(error?.message ?? error ?? '');
  const code = text.match(/\b(AI_[A-Z_]+|MAINTENANCE_IN_PROGRESS)/)?.[1];
  const known = [
    'AI_UNAUTHORIZED', 'AI_NOT_FOUND', 'AI_RATE_LIMITED', 'AI_BAD_REQUEST', 'AI_SERVER_ERROR',
    'AI_NETWORK_ERROR', 'AI_TIMEOUT', 'AI_INVALID_RESPONSE', 'AI_PROVIDER_INCOMPLETE',
    'AI_PROVIDER_INVALID_URL', 'AI_PROVIDER_FIELD_TOO_LONG', 'AI_PROVIDER_KEY_UNREADABLE',
    'AI_NO_PROVIDER', 'AI_BUSY', 'AI_EMPTY_QUESTION', 'AI_PROVIDER_NOT_FOUND', 'MAINTENANCE_IN_PROGRESS',
  ];
  if (text.includes('AUTH_REQUIRED')) return 'ai.errors.AUTH_REQUIRED';
  return known.includes(code) ? `ai.errors.${code}` : 'ai.errors.unknown';
}

/** Detail text after the error code, shown under the friendly message. */
export function aiErrorDetail(error) {
  const text = String(error?.message ?? error ?? '');
  const match = text.match(/AI_[A-Z_]+:\s*(.+)$/s);
  return match ? match[1].trim() : '';
}
