import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import { withAuth } from '../../../lib/auth_api';
import { runMonitorAction, notifySettingsChanged } from '../../../lib/settings_api';
import { useSettingsActivity } from '../SettingsActivityContext';

const CANCELLED = 'Migration cancelled by user';
const idleCancellation = () => ({ pending: false, requested: false, error: '' });

export function useStorageMigration({ storage, onRefresh, t }) {
  const [isMigrating, setIsMigrating] = useState(false);
  const [migrationProgress, setMigrationProgress] = useState({ total_files: 0, copied_files: 0, current_file: '' });
  const [migrationError, setMigrationError] = useState('');
  const [migrationCancellation, setMigrationCancellation] = useState(idleCancellation);
  const [isUpdatingStoragePath, setIsUpdatingStoragePath] = useState(false);
  const [isMigrationChoiceDialogOpen, setIsMigrationChoiceDialogOpen] = useState(false);
  const [pendingTargetPath, setPendingTargetPath] = useState('');
  const mountedRef = useRef(true);
  const migrationRunRef = useRef(null);
  useSettingsActivity('storage-migration', { busy: isMigrating || isUpdatingStoragePath });
  const migrationUnlistenersRef = useRef([]);
  const currentStoragePath = storage?.root_path || 'LocalAppData/CarbonPaper';

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      migrationUnlistenersRef.current.forEach((unlisten) => {
        try { unlisten(); } catch { }
      });
      migrationUnlistenersRef.current = [];
    };
  }, []);

  const executeStoragePathChange = async (targetPath, shouldMigrateData) => {
    if (!targetPath || migrationRunRef.current) return;
    const run = { started: false, finished: false, cancelPending: false, cancelRequested: false };
    migrationRunRef.current = run;
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
      setMigrationError('');
      setMigrationCancellation(idleCancellation());
      setIsUpdatingStoragePath(true);
      try {
        const monitorStatusRaw = await invoke('get_monitor_status');
        const monitorStatus = typeof monitorStatusRaw === 'string' ? JSON.parse(monitorStatusRaw) : monitorStatusRaw;
        shouldRestartMonitor = !monitorStatus?.stopped;
      } catch {
        shouldRestartMonitor = true;
      }

      if (shouldRestartMonitor) {
        await runMonitorAction('maintenance-stop');
      }

      if (shouldMigrateData) {
        setIsMigrating(true);
        setMigrationProgress({ total_files: 0, copied_files: 0, current_file: '' });

        unlistenProgress = await registerMigrationListener('storage-migration-progress', (evt) => {
          setMigrationProgress(evt.payload);
        });

        unlistenError = await registerMigrationListener('storage-migration-error', (evt) => {
          if (!evt.payload?.cancelled) setMigrationError(evt.payload?.message || t('settings.storageManagement.migration.error_default'));
        });
      }

      await withAuth(async () => {
        if (run.cancelRequested) throw new Error(CANCELLED);
        run.started = true;
        try {
          return await invoke('storage_migrate_data_dir', { target: targetPath, migrateDataFiles: shouldMigrateData });
        } catch (error) {
          // A rejected admission can still be cancelled while Hello is open;
          // the retry must not begin work after that cancellation.
          if (String(error).includes('AUTH_REQUIRED')) run.started = false;
          throw error;
        }
      }, { autoPrompt: true });
      run.finished = true;

      if (shouldMigrateData) {
        if (mountedRef.current) {
          setMigrationProgress((s) => ({ ...s, current_file: t('settings.storageManagement.migration.completed') }));
        }
        await new Promise((resolve) => setTimeout(resolve, 600));
      }

      onRefresh?.();
      await notifySettingsChanged(['storage', 'records']);
    } catch (e) {
      if (String(e?.message || e) !== CANCELLED && mountedRef.current) {
        console.error('change storage path failed', e);
        setMigrationError(String(e));
      }
    } finally {
      run.finished = true;
      await removeMigrationListener(unlistenProgress);
      await removeMigrationListener(unlistenError);

      if (mountedRef.current) {
        setIsMigrating(false);
        setIsUpdatingStoragePath(false);
      }
      if (shouldRestartMonitor) {
        try { await runMonitorAction('maintenance-start'); } catch { }
      }
      if (migrationRunRef.current === run) migrationRunRef.current = null;
    }
  };

  const handleCancelMigration = async () => {
    const run = migrationRunRef.current;
    if (!run || run.finished || run.cancelPending || run.cancelRequested) return;
    run.cancelPending = true;
    run.cancelRequested = true;
    setMigrationCancellation({ pending: true, requested: false, error: '' });
    try {
      let accepted = !run.started;
      if (run.started) {
        const result = await withAuth(() => {
          if (migrationRunRef.current !== run || run.finished) return { status: 'idle' };
          return invoke('storage_migration_cancel');
        }, { autoPrompt: true });
        accepted = result?.status === 'cancel_requested' || !run.started || run.finished;
      }
      run.cancelRequested = accepted;
      if (mountedRef.current && migrationRunRef.current === run) {
        setMigrationCancellation({ pending: false, requested: accepted, error: '' });
      }
    } catch (error) {
      run.cancelRequested = false;
      if (mountedRef.current && migrationRunRef.current === run) {
        setMigrationCancellation({ pending: false, requested: false, error: String(error?.message || error) });
      }
    } finally {
      run.cancelPending = false;
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
    migrationCancellation,
    isUpdatingStoragePath,
    isMigrationChoiceDialogOpen,
    pendingTargetPath,
    currentStoragePath,
    handleChangeStoragePath,
    handleCancelMigrationChoice,
    handleApplyStoragePath,
    handleCancelMigration,
  };
}
