import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { formatInvokeError } from '../filterUtils';

export function useAutoLaunchStatus({ isOpen, t }) {
  const [autoLaunchEnabled, setAutoLaunchEnabled] = useState(null);
  const [autoLaunchLoading, setAutoLaunchLoading] = useState(false);
  const [autoLaunchMessage, setAutoLaunchMessage] = useState('');

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
    setAutoLaunchLoading(true);
    setAutoLaunchMessage('');
    try {
      const next = !(autoLaunchEnabled ?? false);
      const result = await invoke('set_autostart', { enabled: next });
      setAutoLaunchEnabled(Boolean(result));
      setAutoLaunchMessage(Boolean(result) ? t('settings.autolaunch.enabled') : t('settings.autolaunch.disabled'));
    } catch (e) {
      setAutoLaunchMessage(t('settings.autolaunch.action_failed', { error: formatInvokeError(e) }));
    } finally {
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
