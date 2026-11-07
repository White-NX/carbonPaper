import React from 'react';
import { useTranslation } from 'react-i18next';
import { ShieldCheck } from 'lucide-react';
import { useHmacMigration } from '../hooks/useHmacMigration';
import { formatError } from '../lib/errors';
import { OverlayShell, ProgressBlock, useOverlaySlot } from './overlay';
import { Button } from './ui/Button';
import { Banner } from './ui/Banner';

export default function HmacMigrationDialog() {
  const { t } = useTranslation();
  const { open, progress, error, dismiss } = useHmacMigration();
  const visible = useOverlaySlot('hmac', open);

  return (
    <OverlayShell
      open={visible}
      onDismiss={dismiss}
      size="lg"
      icon={ShieldCheck}
      title={t('settings.storageManagement.migration.dialog_title')}
      footer={<Button size="md" onClick={dismiss}>{t('common.close')}</Button>}
    >
      {error ? (
        <Banner tone="error" title={t('settings.storageManagement.migration.error_default')}>{formatError(error)}</Banner>
      ) : (
        <>
          <ProgressBlock
            label={t('settings.storageManagement.migration.upgrading')}
            current={progress.processed}
            total={progress.total}
          />
          <Banner tone="info">{t('settings.storageManagement.migration.background_tip')}</Banner>
        </>
      )}
    </OverlayShell>
  );
}
