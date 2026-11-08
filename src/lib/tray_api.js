import { invoke } from '@tauri-apps/api/core';
import { withAuth } from './auth_api';

export function takeTrayNavigation() {
  return withAuth(() => invoke('tray_take_navigation'), { promptMode: 'defer' });
}
