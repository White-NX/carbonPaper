import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import { withAuth } from '../../../lib/auth_api';

export function useStorageMigration({ storage, onRefresh, t }) {
  const [isMigrating, setIsMigrating] = useState(false);
  const [migrationProgress, setMigrationProgress] = useState({ total_files: 0, copied_files: 0, current_file: '' });
  const [migrationError, setMigrationError] = useState('');
  const [isUpdatingStoragePath, setIsUpdatingStoragePath] = useState(false);
  const [isMigrationChoiceDialogOpen, setIsMigrationChoiceDialogOpen] = useState(false);
  const [pendingTargetPath, setPendingTargetPath] = useState('');
  const mountedRef = useRef(true);
  const migrationUnlistenersRef = useRef([]);
  const currentStoragePath = storage?.root_path || 'LocalAppData/CarbonPaper';

  useEffect(() => {
    return () => {
      mountedRef.current = false;
      migrationUnlistenersRef.current.forEach((unlisten) => {
        try { unlisten(); } catch { }
      });
      migrationUnlistenersRef.current = [];
    };
  }, []);

  const executeStoragePathChange = async (targetPath, shouldMigrateData) => {
    let unlistenProgress = null;
    let unlistenError = null;
    let shouldRestartMonitor = true;
    const registerMigrationListener = async (eventName, handler) => {
      const unlisten = await listen(eventName, (evt) => {
        if (mountedRef.current) {
          handler(evt);
        }
      });
      if (!mountedRef.current) {
        try { unlisten(); } catch { }
        return null;
      }
      migrationUnlistenersRef.current.push(unlisten);
      return unlisten;
    };
    const removeMigrationListener = async (unlisten) => {
      if (!unlisten) return;
      migrationUnlistenersRef.current = migrationUnlistenersRef.current.filter((fn) => fn !== unlisten);
      try { await unlisten(); } catch { }
    };

    try {
      if (!targetPath) return;

      setMigrationError('');
      setIsUpdatingStoragePath(true);
      try {
        const monitorStatusRaw = await invoke('get_monitor_status');
        const monitorStatus = typeof monitorStatusRaw === 'string' ? JSON.parse(monitorStatusRaw) : monitorStatusRaw;
        shouldRestartMonitor = !monitorStatus?.stopped;
      } catch {
        shouldRestartMonitor = true;
      }

      if (shouldRestartMonitor) {
        await withAuth(() => invoke('stop_monitor'), { autoPrompt: true });
      }

      if (shouldMigrateData) {
        setIsMigrating(true);
        setMigrationProgress({ total_files: 0, copied_files: 0, current_file: '' });

        unlistenProgress = await registerMigrationListener('storage-migration-progress', (evt) => {
          setMigrationProgress(evt.payload);
        });

        unlistenError = await registerMigrationListener('storage-migration-error', (evt) => {
          setMigrationError(evt.payload?.message || t('settings.storageManagement.migration.error_default'));
        });
      }

      await withAuth(() => invoke('storage_migrate_data_dir', {
        target: targetPath,
        migrateDataFiles: shouldMigrateData,
      }), { autoPrompt: true });

      if (shouldMigrateData) {
        if (mountedRef.current) {
          setMigrationProgress((s) => ({ ...s, current_file: t('settings.storageManagement.migration.completed') }));
        }
        await new Promise((resolve) => setTimeout(resolve, 600));
      }

      onRefresh?.();
    } catch (e) {
      console.error('change storage path failed', e);
      if (mountedRef.current) {
        setMigrationError(String(e));
      }
    } finally {
      await removeMigrationListener(unlistenProgress);
      await removeMigrationListener(unlistenError);

      if (mountedRef.current) {
        setIsMigrating(false);
        setIsUpdatingStoragePath(false);
      }
      if (shouldRestartMonitor) {
        try { await withAuth(() => invoke('start_monitor'), { autoPrompt: true }); } catch { }
      }
    }
  };

  const handleChangeStoragePath = async () => {
    try {
      const selected = await open({ directory: true });
      if (!selected) return;

      const targetPath = Array.isArray(selected) ? selected[0] : selected;
      if (!targetPath) return;

      const normalizedCurrent = currentStoragePath.replace(/[\\/]+$/, '');
      const normalizedTarget = targetPath.replace(/[\\/]+$/, '');
      if (normalizedCurrent && normalizedCurrent === normalizedTarget) {
        return;
      }

      setPendingTargetPath(targetPath);
      setIsMigrationChoiceDialogOpen(true);
    } catch (e) {
      console.error('select storage path failed', e);
      setMigrationError(String(e));
    }
  };

  const handleCancelMigrationChoice = () => {
    setIsMigrationChoiceDialogOpen(false);
    setPendingTargetPath('');
  };

  const handleApplyStoragePath = async (shouldMigrateData) => {
    const targetPath = pendingTargetPath;
    setIsMigrationChoiceDialogOpen(false);
    setPendingTargetPath('');
    await executeStoragePathChange(targetPath, shouldMigrateData);
  };

  return {
    isMigrating,
    migrationProgress,
    migrationError,
    isUpdatingStoragePath,
    isMigrationChoiceDialogOpen,
    pendingTargetPath,
    currentStoragePath,
    handleChangeStoragePath,
    handleCancelMigrationChoice,
    handleApplyStoragePath,
  };
}
