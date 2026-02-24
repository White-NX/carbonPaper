import React from 'react';
import { useTranslation } from 'react-i18next';
import { Dialog } from '../Dialog';
import { invoke } from '@tauri-apps/api/core';

export default function MigrationProgressDialog({ isOpen, onClose, progress, error }) {
  const { t } = useTranslation();
  const percent = progress && progress.total_files > 0 ? Math.round((progress.copied_files / progress.total_files) * 100) : 0;

  return (
    <Dialog isOpen={isOpen} onClose={onClose} title={t('settings.storageManagement.migration.dialog_title')} maxWidth="max-w-xl">
      <div className="p-4 space-y-3">
        {error ? (
          <div className="text-sm text-ide-error">{t('settings.storageManagement.migration.error_default')}: {error}</div>
        ) : (
          <>
            <div className="text-sm text-ide-muted">{t('settings.storageManagement.migration.copying', { copied: progress.copied_files, total: progress.total_files })}</div>
            <div className="w-full bg-ide-bg border border-ide-border rounded overflow-hidden h-3">
              <div className="bg-ide-accent h-3" style={{ width: `${percent}%` }} />
            </div>
            <div className="text-xs text-ide-muted">{t('settings.storageManagement.migration.current_file', { file: progress.current_file || '--' })}</div>
          </>
        )}

        <div className="flex items-center justify-end gap-2 pt-3">
          {!error && (
            <button
              className="px-3 py-1 text-sm rounded border border-ide-border bg-ide-panel"
              onClick={async () => {
                try {
                  await invoke('storage_migration_cancel');
                } catch (e) {
                  // ignore if not implemented
                }
              }}
            >取消</button>
          )}
        </div>
      </div>
    </Dialog>
  );
}
