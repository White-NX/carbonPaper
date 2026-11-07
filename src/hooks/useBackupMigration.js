import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { withAuth } from '../lib/auth_api';
import { formatError } from '../lib/errors';
import { selectBackupExportPath, selectBackupImportPath } from '../lib/native_dialogs';
import { notifySettingsChanged } from '../lib/settings_api';
import { useSettingsActivity } from '../components/settings/SettingsActivityContext';
import { useTauriEventListener } from './useTauriEventListener';

const CANCELLED = 'BACKUP_CANCELLED';
const backupErrors = {
  BACKUP_INCORRECT_PASSWORD_OR_CORRUPT: 'settings.backup.error_password_or_corrupt',
  BACKUP_LEGACY_DEVICE_REQUIRED: 'settings.backup.error_legacy_device',
  BACKUP_UNSUPPORTED_FORMAT: 'settings.backup.error_unsupported_format',
  BACKUP_EXPORT_TARGET_CONFLICT: 'settings.backup.error_export_target',
  BACKUP_UNREFERENCED_PLAINTEXT: 'settings.backup.error_legacy_images',
};

export function useBackupMigration({ mode, t, onClose }) {
  const [password, setPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [status, setStatus] = useState('idle');
  const [error, setError] = useState('');
  const [progress, setProgress] = useState(null);
  const [cancelState, setCancelState] = useState('idle');
  const runRef = useRef(null);
  const mountedRef = useRef(true);
  useSettingsActivity('backup', { busy: status === 'processing' });

  useEffect(() => {
    mountedRef.current = true;
    return () => { mountedRef.current = false; };
  }, []);

  useTauriEventListener('backup-migration-progress', (event) => {
    if (runRef.current && !runRef.current.finished) setProgress(event.payload);
  }, [], status === 'processing');

  const showIdle = () => {
    setStatus('idle');
    setProgress(null);
    setCancelState('idle');
  };

  const handleAction = async () => {
    if (runRef.current) return;
    if (mode === 'export' && password !== confirmPassword) {
      setError(t('settings.backup.error_password_mismatch'));
      return;
    }
    if (!password) {
      setError(t('settings.backup.error_password_empty'));
      return;
    }

    const run = { started: false, finished: false, cancelPending: false, cancelRequested: false, cancelAccepted: false };
    runRef.current = run;
    setError('');
    setProgress(null);
    setCancelState('idle');
    setStatus('processing');
    try {
      const filePath = await (mode === 'export' ? selectBackupExportPath() : selectBackupImportPath());
      if (!filePath || run.cancelRequested) {
        if (mountedRef.current) showIdle();
        return;
      }

      await withAuth(async () => {
        // A cancelled picker or authentication retry must never admit new work.
        if (run.cancelRequested) throw new Error(CANCELLED);
        run.started = true;
        try {
          return await invoke(mode === 'export' ? 'storage_export_backup' : 'storage_import_backup',
            mode === 'export' ? { password, exportPath: filePath } : { password, backupZipPath: filePath });
        } catch (failure) {
          if (formatError(failure).includes('AUTH_REQUIRED')) run.started = false;
          throw failure;
        }
      }, { autoPrompt: true });

      run.finished = true;
      if (mountedRef.current) setStatus('success');
      if (mode === 'import') {
        try { await notifySettingsChanged(['storage', 'records']); }
        catch (failure) { console.warn('Failed to notify settings after backup import:', failure); }
      }
    } catch (failure) {
      if (!mountedRef.current) return;
      const message = formatError(failure);
      if (message.includes(CANCELLED)) {
        showIdle();
        return;
      }
      const key = Object.keys(backupErrors).find(code => message.includes(code));
      setError(key ? t(backupErrors[key]) : message);
      setStatus('error');
    } finally {
      run.finished = true;
      if (runRef.current === run) runRef.current = null;
    }
  };

  const handleCancel = async () => {
    const run = runRef.current;
    if (!run || run.finished || run.cancelPending || run.cancelAccepted) return;
    run.cancelPending = true;
    // Keep this intent through authentication, even if admission is rejected.
    run.cancelRequested = true;
    setError('');
    setCancelState('pending');
    try {
      const accepted = !run.started || await withAuth(() => {
        if (run.finished || runRef.current !== run) return false;
        if (!run.started) return true;
        return invoke('storage_backup_cancel');
      }, { autoPrompt: true });
      run.cancelAccepted = accepted;
      if (mountedRef.current && runRef.current === run && !run.finished) {
        setCancelState(accepted ? 'pending' : 'finishing');
      }
    } catch (failure) {
      if (mountedRef.current && runRef.current === run && !run.finished) {
        setCancelState('idle');
        setError(formatError(failure));
      }
    } finally {
      run.cancelPending = false;
    }
  };

  const handleClose = () => {
    if (runRef.current) return;
    setPassword('');
    setConfirmPassword('');
    setError('');
    showIdle();
    onClose();
  };

  const handleRetry = () => {
    setError('');
    showIdle();
  };

  return {
    password, setPassword, confirmPassword, setConfirmPassword,
    status, error, progress, cancelState, handleAction, handleCancel, handleClose, handleRetry,
  };
}
