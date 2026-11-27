import React from 'react';
import { useTranslation } from 'react-i18next';
import { ConfirmDialog } from './ConfirmDialog';
import { ProgressBlock } from './overlay/ProgressBlock';

export default function AppBoundRepairFeedback({ repair, ownerOverlay }) {
  const { t } = useTranslation();
  const size = repair.downloadRequest?.sizeBytes;
  const hasSize = Number.isFinite(size) && size > 0;
  return (
    <>
      {repair.busy && <ProgressBlock
        label={t(`appBound.repairProgress.${repair.progress?.phase || 'checking'}`)}
        percent={repair.progress?.total > 0 ? repair.progress.downloaded / repair.progress.total * 100 : undefined}
      />}
      <ConfirmDialog
        ownerOverlay={ownerOverlay}
        isOpen={Boolean(repair.downloadRequest)} title={t('appBound.fullRepairTitle')}
        message={hasSize ? t('appBound.fullRepairDownloadSize', { size: Math.ceil(size / 1024 / 1024) })
          : t('appBound.fullRepairDownload')}
        confirmLabel={t('appBound.downloadAndRepair')} cancelLabel={t('appBound.later')}
        onConfirm={repair.confirmDownload} onCancel={repair.cancelDownload}
      />
    </>
  );
}
