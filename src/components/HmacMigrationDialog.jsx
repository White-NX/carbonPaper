import React, { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Dialog } from './Dialog';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

export default function HmacMigrationDialog() {
  const { t } = useTranslation();
  const [isOpen, setIsOpen] = useState(false);
  const [progress, setProgress] = useState({ processed: 0, total: 0 });
  const [error, setError] = useState(null);

  useEffect(() => {
    let unlisten = null;

    const checkAndRun = async () => {
      try {
        const status = await invoke('storage_check_hmac_migration_status');
        
        if (!status.needs_migration) return;

        setIsOpen(true);
        
        unlisten = await listen('hmac-migration-progress', (event) => {
          setProgress(event.payload);
        });

        if (!status.is_running) {
          await invoke('storage_run_hmac_migration');
        } else {
          try {
            await invoke('storage_run_hmac_migration');
          } catch (e) {
            if (e !== 'ALREADY_RUNNING') throw e;
          }
        }
        
        setIsOpen(false);
      } catch (err) {
        console.error('[HMAC_MIGRATE] Error:', err);
        if (isOpen) setError(err.toString());
      }
    };

    checkAndRun();

    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  if (!isOpen) return null;

  const percent = progress.total > 0 ? Math.round((progress.processed / progress.total) * 100) : 0;

  return (
    <Dialog 
      isOpen={isOpen} 
      onClose={() => setIsOpen(false)} 
      title={t('settings.storageManagement.migration.dialog_title', 'Security Update')} 
      maxWidth="max-w-xl"
    >
      <div className="p-4 space-y-4">
        {error ? (
          <div className="p-3 bg-red-500/10 border border-red-500/20 rounded text-sm text-red-500">
            <strong>Update Error:</strong> {error}
          </div>
        ) : (
          <>
            <div className="space-y-1">
              <div className="text-sm font-medium flex justify-between">
                <span>{t('settings.storageManagement.migration.upgrading', 'Upgrading secure index...')}</span>
                <span className="text-ide-muted">{progress.processed} / {progress.total}</span>
              </div>
              <div className="w-full bg-ide-bg border border-ide-border rounded overflow-hidden h-3">
                <div 
                  className="bg-ide-accent h-3 transition-all duration-300" 
                  style={{ width: progress.total > 0 ? `${percent}%` : '0%' }} 
                />
              </div>
            </div>
            
            <div className="p-3 bg-ide-accent/5 border border-ide-accent/10 rounded-md">
              <p className="text-xs text-ide-muted leading-relaxed">
                {t('settings.storageManagement.migration.background_tip', 'You can safely close this window. The upgrade will continue in the background, and search results will gradually populate.')}
              </p>
            </div>

            <div className="flex justify-end">
              <button 
                onClick={() => setIsOpen(false)}
                className="px-4 py-1.5 bg-ide-bg border border-ide-border rounded text-sm hover:bg-ide-hover transition-colors"
              >
                {t('common.close', 'Close')}
              </button>
            </div>
          </>
        )}
      </div>
    </Dialog>
  );
}
