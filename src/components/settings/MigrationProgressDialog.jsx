import React from 'react';
import { useTranslation } from 'react-i18next';
import { Dialog } from '../Dialog';
import { ProgressBlock } from '../overlay';
import { Banner } from '../ui/Banner';
import { Button } from '../ui/Button';

export default function MigrationProgressDialog({ isOpen, onCancel, progress = {}, error, cancellation = {} }) {
  const { t } = useTranslation();
  const cancelling = cancellation.pending || cancellation.requested;

  return (
    <Dialog isOpen={isOpen} onClose={onCancel} disableClose title={t('settings.storageManagement.migration.dialog_title')} maxWidth="max-w-xl">
      <div className="space-y-3 p-4">
        {error && <Banner tone="error">{t('settings.storageManagement.migration.error_default')}: {error}</Banner>}
        <ProgressBlock
          label={t('settings.storageManagement.migration.copying', { copied: progress.copied_files || 0, total: progress.total_files || 0 })}
          current={progress.copied_files} total={progress.total_files} showCount={false}
          detail={progress.current_file ? t('settings.storageManagement.migration.current_file', { file: progress.current_file }) : undefined}
        />
        {cancellation.error && <Banner tone="error">{t('settings.storageManagement.migration.cancel_failed', { error: cancellation.error })}</Banner>}
        <div className="flex justify-end pt-3">
          <Button onClick={onCancel} loading={Boolean(cancelling)}>
            {cancelling ? t('settings.storageManagement.migration.cancelling') : t('common.cancel')}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}
