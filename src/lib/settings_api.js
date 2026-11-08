import { invoke } from '@tauri-apps/api/core';
import { emitTo } from '@tauri-apps/api/event';
import { withAuth } from './auth_api';

export function setAppTheme(theme) {
  return invoke('set_app_theme', { theme });
}

export function isSettingsWindow() {
  return typeof window !== 'undefined' && new URLSearchParams(window.location.search).get('window') === 'settings';
}

export function openSettingsWindow(tab, section) {
  const target = { tab: tab || localStorage.getItem('settings.lastTab') || 'general', section: section || null };
  // The native open command belongs to the main window. Navigation inside an
  // existing settings window stays local and preserves the visited page drafts.
  if (isSettingsWindow()) return emitTo('settings', 'settings-navigate', target);
  return invoke('open_settings_window', target);
}

export function notifySettingsChanged(keys) {
  window.dispatchEvent(new CustomEvent('settings-preferences-changed', { detail: keys }));
  return invoke('settings_preferences_changed', { keys }).catch((error) => {
    console.warn('Failed to notify other windows about settings:', error);
  });
}

export async function saveAdvancedConfig(patch, authOptions = { autoPrompt: true }) {
  await withAuth(() => invoke('set_advanced_config', { config: patch }), authOptions);
  await notifySettingsChanged(['advanced']);
}

export async function runMonitorAction(action) {
  if (!['start', 'stop', 'pause', 'resume', 'restart'].includes(action)) throw new Error('INVALID_MONITOR_ACTION');
  return invoke('settings_monitor_action', { action });
}
