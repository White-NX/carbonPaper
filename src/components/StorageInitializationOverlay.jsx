import React from 'react';
import { Database } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { LogDisclosure, OverlayShell, ProgressBlock, useOverlaySlot } from './overlay';
import { Banner } from './ui/Banner';
import { Button } from './ui/Button';

const PHASE_LABELS = {
  credentials: 'storageStartup.phases.credentials',
  opening: 'storageStartup.phases.opening',
  schema: 'storageStartup.phases.schema',
  metadata: 'storageStartup.phases.metadata',
  vectors: 'storageStartup.phases.vectors',
  processes: 'storageStartup.phases.processes',
  cleanup: 'storageStartup.phases.cleanup',
  compacting: 'storageStartup.phases.compacting',
  finalizing: 'storageStartup.phases.finalizing',
};

export default function StorageInitializationOverlay({ initialization }) {
  const { t } = useTranslation();
  const { status, showOverlay, error, retrying, retry } = initialization;
  const visible = useOverlaySlot('storageInitialization', showOverlay);
  return (
    <OverlayShell
      open={visible}
      layer="maintenance"
      size="sm"
      icon={Database}
      title={t('storageStartup.title')}
      footer={error ? <Button size="md" loading={retrying} onClick={retry}>{t('common.retry')}</Button> : undefined}
    >
      {error ? (
        <>
          <Banner tone="error">{t('storageStartup.error')}</Banner>
          <LogDisclosure error lines={[error]} />
        </>
      ) : (
        <>
          <ProgressBlock
            label={t(PHASE_LABELS[status.phase] || 'storageStartup.phases.opening')}
            current={status.processed}
            total={status.total}
          />
          <p className="text-xs leading-relaxed text-ide-muted">{t('storageStartup.tip')}</p>
        </>
      )}
    </OverlayShell>
  );
}
