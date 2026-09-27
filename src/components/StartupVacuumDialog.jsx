import React, { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { Database } from 'lucide-react';
import { usePolling } from '../hooks/usePolling';
import { OverlayShell, ProgressBlock, useOverlaySlot } from './overlay';

export default function StartupVacuumDialog() {
  const { t } = useTranslation();
  const [isOpen, setIsOpen] = useState(false);
  const [polling, setPolling] = useState(false);

  useEffect(() => {
    let mounted = true;

    const checkAndRun = async () => {
      try {
        const status = await invoke('storage_get_startup_vacuum_status');
        if (!mounted) return;

        if (status?.in_progress) {
          setIsOpen(true);
          setPolling(true);
          return;
        }

        if (!status?.needs_vacuum) {
          setIsOpen(false);
          return;
        }

        setIsOpen(true);
        const result = await invoke('storage_run_startup_vacuum_if_needed');
        if (!mounted) return;

        if (result?.already_running) {
          setPolling(true);
          return;
        }

        setIsOpen(false);
      } catch (err) {
        console.warn('[VACUUM] Startup vacuum failed:', err);
        if (mounted) setIsOpen(false);
      }
    };

    checkAndRun();
    return () => { mounted = false; };
  }, []);

  usePolling(async () => {
    try {
      const status = await invoke('storage_get_startup_vacuum_status');
      const inProgress = Boolean(status?.in_progress);
      setIsOpen(inProgress);
      if (!inProgress) setPolling(false);
      return inProgress;
    } catch (err) {
      console.warn('[VACUUM] Failed to poll status:', err);
      setIsOpen(false);
      setPolling(false);
      return false;
    }
  }, { intervalMs: 1000, enabled: polling });

  const visible = useOverlaySlot('vacuum', isOpen);

  return (
    <OverlayShell
      open={visible}
      layer="maintenance"
      size="sm"
      icon={Database}
      title={t('settings.storageManagement.vacuum.dialog_title')}
    >
      <ProgressBlock label={t('settings.storageManagement.vacuum.running')} />
      <p className="text-xs leading-relaxed text-ide-muted">{t('settings.storageManagement.vacuum.tip')}</p>
    </OverlayShell>
  );
}
