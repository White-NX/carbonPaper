import React from 'react';
import { useTranslation } from 'react-i18next';
import { Dialog } from './Dialog';
import SnapshotViewer from './SnapshotViewer';
import { useDialogVisibility } from '../hooks/useDialogFocus';
import { useSnapshotPreviewData } from '../hooks/useSnapshotPreviewData';

export default function SnapshotPreviewDialog({ item, onClose, onOpenTimeline, onOpenStandalone, onOpenRecap, closeLabel }) {
  const { t } = useTranslation();
  const visible = useDialogVisibility(Boolean(item));
  const data = useSnapshotPreviewData(item, visible);
  return (
    <Dialog isOpen={Boolean(item)} onClose={onClose} title={t('snapshotPreview.windowTitle')} hideCloseButton
      maxWidth="max-w-[1280px]" className="h-[86vh]" contentClassName="flex flex-col overflow-hidden">
      <SnapshotViewer item={item} imageSrc={data.imageSrc} details={data.details} loading={data.loading} error={data.error} onRetry={data.retry}
        onClose={onClose} closeLabel={closeLabel} onOpenTimeline={onOpenTimeline} onOpenStandalone={onOpenStandalone} onOpenRecap={onOpenRecap} />
    </Dialog>
  );
}
