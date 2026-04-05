import React, { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { Loader2 } from 'lucide-react';
import { Dialog } from './Dialog';

export default function StartupVacuumDialog() {
  const { t } = useTranslation();
  const [isOpen, setIsOpen] = useState(false);
  const pollTimerRef = useRef(null);

  const stopPolling = () => {
    if (pollTimerRef.current) {
      clearInterval(pollTimerRef.current);
      pollTimerRef.current = null;
    }
  };

  useEffect(() => {
    let mounted = true;

    const pollVacuumStatus = async () => {
      try {
        const status = await invoke('storage_get_startup_vacuum_status');
        if (!mounted) return;

        const inProgress = Boolean(status?.in_progress);
        setIsOpen(inProgress);

        if (!inProgress) {
          stopPolling();
        }
      } catch (err) {
        console.warn('[VACUUM] Failed to poll status:', err);
        if (mounted) {
          setIsOpen(false);
        }
        stopPolling();
      }
    };

    const startPolling = () => {
      if (pollTimerRef.current) return;
      pollTimerRef.current = setInterval(pollVacuumStatus, 1000);
    };

    const checkAndRun = async () => {
      try {
        const status = await invoke('storage_get_startup_vacuum_status');
        if (!mounted) return;

        if (status?.in_progress) {
          setIsOpen(true);
          startPolling();
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
          startPolling();
          return;
        }

        setIsOpen(false);
      } catch (err) {
        console.warn('[VACUUM] Startup vacuum failed:', err);
        if (mounted) {
          setIsOpen(false);
        }
      }
    };

    checkAndRun();

    return () => {
      mounted = false;
      stopPolling();
    };
  }, []);

  if (!isOpen) return null;

  return (
    <Dialog
      isOpen={isOpen}
      onClose={() => {}}
      title={t('settings.storageManagement.vacuum.dialog_title', '数据库优化中')}
      maxWidth="max-w-md"
      disableClose
      hideCloseButton
    >
      <div className="p-5 space-y-4">
        <div className="flex items-start gap-3">
          <Loader2 className="w-5 h-5 mt-0.5 text-ide-accent animate-spin" />
          <div className="space-y-1">
            <p className="text-sm font-medium text-ide-text">
              {t('settings.storageManagement.vacuum.running', '正在执行 VACUUM 数据库优化，请稍候...')}
            </p>
            <p className="text-xs text-ide-muted leading-relaxed">
              {t('settings.storageManagement.vacuum.tip', '优化期间将暂时锁定数据库，窗口不可关闭。完成后会自动恢复。')}
            </p>
          </div>
        </div>
      </div>
    </Dialog>
  );
}
