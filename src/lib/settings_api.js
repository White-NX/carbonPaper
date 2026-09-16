import { invoke } from '@tauri-apps/api/core';
import { withAuth } from './auth_api';

export function isSettingsWindow() {
  return typeof window !== 'undefined' && new URLSearchParams(window.location.search).get('window') === 'settings';
}

export function openSettingsWindow(tab, section) {
  return invoke('open_settings_window', { tab: tab || localStorage.getItem('settings.lastTab') || 'general', section: section || null });
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
