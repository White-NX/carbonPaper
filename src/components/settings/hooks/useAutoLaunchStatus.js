import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { formatInvokeError } from '../filterUtils';
import { withAuth } from '../../../lib/auth_api';
import { useSettingsActivity } from '../SettingsActivityContext';

export function useAutoLaunchStatus({ isOpen, t }) {
  const [autoLaunchEnabled, setAutoLaunchEnabled] = useState(null);
  const [autoLaunchLoading, setAutoLaunchLoading] = useState(false);
  const [autoLaunchMessage, setAutoLaunchMessage] = useState('');
  const [saving, setSaving] = useState(false);
  useSettingsActivity('auto-launch', { busy: saving });

  const refreshAutoLaunchStatus = async () => {
    setAutoLaunchLoading(true);
    setAutoLaunchMessage('');
    try {
      const enabled = await invoke('get_autostart_status');
      setAutoLaunchEnabled(Boolean(enabled));
    } catch (e) {
      setAutoLaunchMessage(e?.message || t('settings.autolaunch.read_error'));
      setAutoLaunchEnabled(null);
    } finally {
      setAutoLaunchLoading(false);
    }
  };

  const handleToggleAutoLaunch = async () => {
    if (saving) return;
    setSaving(true);
    setAutoLaunchLoading(true);
    setAutoLaunchMessage('');
    try {
      const next = !(autoLaunchEnabled ?? false);
      const result = await withAuth(
        () => invoke('set_autostart', { enabled: next }),
        { autoPrompt: true },
      );
      setAutoLaunchEnabled(Boolean(result));
      setAutoLaunchMessage(Boolean(result) ? t('settings.autolaunch.enabled') : t('settings.autolaunch.disabled'));
    } catch (e) {
      setAutoLaunchMessage(t('settings.autolaunch.action_failed', { error: formatInvokeError(e) }));
    } finally {
      setSaving(false);
      setAutoLaunchLoading(false);
    }
  };

  useEffect(() => {
    if (isOpen) {
      refreshAutoLaunchStatus();
    }
  }, [isOpen]);

  return {
    autoLaunchEnabled,
    autoLaunchLoading,
    autoLaunchMessage,
    handleToggleAutoLaunch,
  };
}
