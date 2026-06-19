import React, { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open, save } from '@tauri-apps/plugin-dialog';
import { Dialog } from './Dialog';
import { Lock, FileUp, FileDown, ShieldAlert, CheckCircle2, RefreshCw } from 'lucide-react';
import { withAuth } from '../lib/auth_api';

export default function BackupMigrationDialog({ isOpen, onClose, mode = 'export' }) {
  const { t } = useTranslation();
  const [password, setPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [status, setStatus] = useState('idle'); // idle, processing, success, error
  const [error, setError] = useState('');
  const [progress, setProgress] = useState(null);

  useEffect(() => {
    let unlistenPromise;
    if (status === 'processing') {
      unlistenPromise = listen('backup-migration-progress', (event) => {
        setProgress(event.payload);
      });
    }
    return () => {
      if (unlistenPromise) {
        unlistenPromise.then(unlisten => unlisten());
      }
    };
  }, [status]);

  const handleAction = async () => {
    if (mode === 'export' && password !== confirmPassword) {
      setError(t('settings.backup.error_password_mismatch', 'Passwords do not match'));
      return;
    }
    if (!password) {
      setError(t('settings.backup.error_password_empty', 'Password cannot be empty'));
      return;
    }

    try {
      setError('');
      setProgress(null);
      setStatus('processing');

      if (mode === 'export') {
        const filePath = await save({
          filters: [{ name: 'CarbonPaper Backup', extensions: ['zip'] }],
          defaultPath: 'CarbonPaper_Backup.zip'
        });
        if (!filePath) {
          setStatus('idle');
          return;
        }

        await withAuth(() => invoke('storage_export_backup', { password, exportPath: filePath }), { autoPrompt: true });
      } else {
        const filePath = await open({
          filters: [{ name: 'CarbonPaper Backup', extensions: ['zip'] }],
          multiple: false,
          directory: false
        });
        if (!filePath) {
          setStatus('idle');
          return;
        }

        await withAuth(() => invoke('storage_import_backup', { password, backupZipPath: filePath }), { autoPrompt: true });
      }

      setStatus('success');
    } catch (err) {
      console.error('Backup migration failed:', err);
      setStatus('error');
      setError(typeof err === 'string' ? err : (err?.message || String(err)));
    }
  };

  const reset = () => {
    setPassword('');
    setConfirmPassword('');
    setStatus('idle');
    setError('');
    setProgress(null);
  };

  const handleClose = () => {
    if (status === 'processing') return;
    reset();
    onClose();
  };

  const title = mode === 'export' 
    ? t('settings.backup.export_title', 'Export Data Backup') 
    : t('settings.backup.import_title', 'Import Data Backup');

  return (
    <Dialog isOpen={isOpen} onClose={handleClose} title={title} disableClose={status === 'processing'}>
      <div className="p-6 space-y-6">
        {status === 'idle' && (
          <>
            <div className="p-4 bg-ide-accent/5 border border-ide-accent/10 rounded-xl flex gap-3 items-start">
              <ShieldAlert className="w-5 h-5 text-ide-accent shrink-0 mt-0.5" />
              <div className="text-xs text-ide-muted leading-relaxed">
                {mode === 'export' 
                  ? t('settings.backup.export_hint', 'This will pack your database and screenshots into an encrypted ZIP. The password is required to decrypt the backup on another computer. PLEASE KEEP IT SAFE.')
                  : t('settings.backup.import_hint', 'Importing will replace your current data with the contents of the backup. This action cannot be undone.')}
              </div>
            </div>

            <div className="space-y-4">
              <div className="space-y-1.5">
                <label className="text-xs font-medium text-ide-muted ml-1">{t('settings.backup.password_label', 'Backup Password')}</label>
                <div className="relative">
                  <input
                    type="password"
                    value={password}
                    onChange={(e) => setPassword(e.target.value)}
                    className="w-full bg-ide-panel border border-ide-border rounded-lg pl-9 pr-3 py-2 text-sm focus:outline-none focus:border-ide-accent"
                    placeholder="Enter Password"
                  />
                  <Lock className="absolute left-3 top-2.5 w-4 h-4 text-ide-muted" />
                </div>
              </div>

              {mode === 'export' && (
                <div className="space-y-1.5">
                  <label className="text-xs font-medium text-ide-muted ml-1">{t('settings.backup.confirm_password_label', 'Confirm Password')}</label>
                  <div className="relative">
                    <input
                      type="password"
                      value={confirmPassword}
                      onChange={(e) => setConfirmPassword(e.target.value)}
                      className="w-full bg-ide-panel border border-ide-border rounded-lg pl-9 pr-3 py-2 text-sm focus:outline-none focus:border-ide-accent"
                      placeholder="Confirm Password"
                    />
                    <Lock className="absolute left-3 top-2.5 w-4 h-4 text-ide-muted" />
                  </div>
                </div>
              )}
            </div>

            {error && (
              <div className="p-3 bg-red-500/10 border border-red-500/20 rounded-lg text-xs text-red-500">
                {error}
              </div>
            )}

            <div className="flex justify-end gap-3 pt-2">
              <button
                onClick={handleClose}
                className="px-4 py-2 border border-ide-border rounded-lg text-sm hover:bg-ide-panel transition-colors"
              >
                {t('common.cancel', 'Cancel')}
              </button>
              <button
                onClick={handleAction}
                className="px-4 py-2 bg-ide-accent text-white rounded-lg text-sm font-medium hover:bg-ide-accent/90 transition-colors flex items-center gap-2"
              >
                {mode === 'export' ? <FileDown className="w-4 h-4" /> : <FileUp className="w-4 h-4" />}
                {mode === 'export' ? t('settings.backup.export_btn', 'Start Export') : t('settings.backup.import_btn', 'Start Import')}
              </button>
            </div>
          </>
        )}

        {status === 'processing' && (
          <div className="py-8 flex flex-col items-center justify-center space-y-4">
            <RefreshCw className="w-10 h-10 text-ide-accent animate-spin" />
            <div className="text-center w-full max-w-sm">
              <div className="text-sm font-medium">{t('settings.backup.processing', 'Processing...')}</div>
              {progress && progress.total_files > 0 ? (
                <div className="mt-4 space-y-2">
                  <div className="flex items-center justify-between text-xs text-ide-muted">
                    <span className="truncate max-w-[200px] text-left">{progress.current_file}</span>
                    <span className="shrink-0 ml-4">{progress.copied_files} / {progress.total_files}</span>
                  </div>
                  <div className="w-full bg-ide-panel border border-ide-border rounded-full h-2 overflow-hidden">
                    <div 
                      className="bg-ide-accent h-full transition-all duration-300 ease-out" 
                      style={{ width: `${Math.max(2, (progress.copied_files / progress.total_files) * 100)}%` }} 
                    />
                  </div>
                </div>
              ) : (
                <div className="text-xs text-ide-muted mt-1">{t('settings.backup.processing_hint', 'Please do not close the application.')}</div>
              )}
            </div>
          </div>
        )}

        {status === 'success' && (
          <div className="py-6 flex flex-col items-center justify-center space-y-4">
            <CheckCircle2 className="w-12 h-12 text-green-500" />
            <div className="text-center">
              <div className="text-sm font-medium">{t('settings.backup.success_title', 'Operation Successful')}</div>
              <p className="text-xs text-ide-muted mt-2 max-w-[280px]">
                {mode === 'export' 
                  ? t('settings.backup.export_success_hint', 'Backup file has been saved.')
                  : t('settings.backup.import_success_hint', 'Data has been imported. Please restart the application to apply changes.')}
              </p>
            </div>
            <button
              onClick={handleClose}
              className="mt-4 px-6 py-2 bg-ide-panel border border-ide-border rounded-lg text-sm font-medium hover:bg-ide-hover transition-colors"
            >
              {t('common.close', 'Close')}
            </button>
          </div>
        )}

        {status === 'error' && (
          <div className="py-6 flex flex-col items-center justify-center space-y-4">
            <div className="w-12 h-12 rounded-full bg-red-500/10 flex items-center justify-center">
              <ShieldAlert className="w-7 h-7 text-red-500" />
            </div>
            <div className="text-center">
              <div className="text-sm font-medium text-red-500">{t('settings.backup.error_title', 'Operation Failed')}</div>
              <p className="text-xs text-ide-muted mt-2 break-all max-w-[300px]">
                {error}
              </p>
            </div>
            <div className="flex gap-3 pt-2">
              <button
                onClick={() => setStatus('idle')}
                className="px-4 py-2 border border-ide-border rounded-lg text-sm hover:bg-ide-panel transition-colors"
              >
                {t('common.retry', 'Retry')}
              </button>
              <button
                onClick={handleClose}
                className="px-4 py-2 bg-ide-panel border border-ide-border rounded-lg text-sm hover:bg-ide-hover transition-colors"
              >
                {t('common.close', 'Close')}
              </button>
            </div>
          </div>
        )}
      </div>
    </Dialog>
  );
}
