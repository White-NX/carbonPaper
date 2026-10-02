import { invoke } from '@tauri-apps/api/core';
import { emitTo } from '@tauri-apps/api/event';
import { withAuth } from './auth_api';

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

export async function saveAdvancedConfig(patch) {
  await withAuth(() => invoke('set_advanced_config', { config: patch }), { autoPrompt: true });
  await notifySettingsChanged(['advanced']);
}

export async function runMonitorAction(action) {
  if (isSettingsWindow()) return invoke('settings_monitor_action', { action });
  const commands = { start: 'start_monitor', stop: 'stop_monitor', pause: 'pause_monitor', resume: 'resume_monitor', 'maintenance-stop': 'stop_monitor', 'maintenance-start': 'start_monitor' };
  if (action === 'restart') {
    await invoke('stop_monitor');
    return invoke('start_monitor');
  }
  if (!commands[action]) throw new Error('INVALID_MONITOR_ACTION');
  return invoke(commands[action]);
}
