import React from 'react';
import { useTranslation } from 'react-i18next';
import BackupMigrationDialog from '../../BackupMigrationDialog';
import { ConfirmDialog } from '../../ConfirmDialog';
import { Dialog } from '../../Dialog';
import MigrationProgressDialog from '../MigrationProgressDialog';

export default function StorageDialogs({
  pendingDeleteIntent,
  deletingTarget,
  onCancelSoftDelete,
  onConfirmSoftDelete,
  isMigrating,
  migrationProgress,
  migrationError,
  isMigrationChoiceDialogOpen,
  pendingTargetPath,
  onCancelMigrationChoice,
  onApplyStoragePath,
  isBackupDialogOpen,
  onCloseBackupDialog,
  backupMode,
}) {
  const { t } = useTranslation();

  return (
    <>
      <ConfirmDialog
        isOpen={Boolean(pendingDeleteIntent)}
        onCancel={onCancelSoftDelete}
        onConfirm={onConfirmSoftDelete}
        title={pendingDeleteIntent?.title || t('settings.storageManagement.deleteConfirm.title')}
        message={pendingDeleteIntent?.message || ''}
        confirmLabel={pendingDeleteIntent?.confirmLabel || t('settings.storageManagement.deleteConfirm.confirmDefault')}
        cancelLabel={t('settings.storageManagement.deleteConfirm.cancel')}
        confirmVariant="danger"
        loading={Boolean(pendingDeleteIntent && deletingTarget === pendingDeleteIntent.targetKey)}
      />

      <MigrationProgressDialog
        isOpen={isMigrating}
        onClose={() => { /* prevent closing while migrating */ }}
        progress={migrationProgress}
        error={migrationError}
      />

      <Dialog
        isOpen={isMigrationChoiceDialogOpen}
        onClose={onCancelMigrationChoice}
        title={t('settings.storageManagement.storagePath.changeTitle')}
        maxWidth="max-w-md"
      >
        <div className="p-4 space-y-3">
          <div className="text-sm text-ide-text">{t('settings.storageManagement.storagePath.selectedPath')}</div>
          <div className="px-3 py-2 rounded-lg border border-ide-border bg-ide-panel text-xs text-ide-muted break-all">
            {pendingTargetPath || '--'}
          </div>
          <p className="text-xs text-ide-muted">{t('settings.storageManagement.storagePath.migrateQuestion')}</p>
          <div className="flex items-center justify-end gap-2 pt-2">
            <button
              type="button"
              onClick={onCancelMigrationChoice}
              className="px-3 py-1.5 text-xs border border-ide-border rounded-lg bg-ide-panel hover:border-ide-accent hover:text-ide-accent transition-colors"
            >
              {t('settings.storageManagement.storagePath.cancel')}
            </button>
            <button
              type="button"
              onClick={() => onApplyStoragePath(false)}
              className="px-3 py-1.5 text-xs border border-ide-border rounded-lg bg-ide-panel hover:border-ide-accent hover:text-ide-accent transition-colors"
            >
              {t('settings.storageManagement.storagePath.applyPath')}
            </button>
            <button
              type="button"
              onClick={() => onApplyStoragePath(true)}
              className="px-3 py-1.5 text-xs rounded-lg bg-ide-accent hover:bg-ide-accent/90 text-white transition-colors"
            >
              {t('settings.storageManagement.storagePath.migrateAndApply')}
            </button>
          </div>
        </div>
      </Dialog>

      <BackupMigrationDialog
        isOpen={isBackupDialogOpen}
        onClose={onCloseBackupDialog}
        mode={backupMode}
      />
    </>
  );
}
