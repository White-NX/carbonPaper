import React from 'react';
import { useTranslation } from 'react-i18next';
import { Download, RotateCcw } from 'lucide-react';
import { LogDisclosure, OverlayShell, ProgressBlock, useOverlaySlot } from './overlay';
import { Button } from './ui/Button';
import { Banner } from './ui/Banner';

/**
 * The standalone required-model download card. The download itself runs in
 * `useRequiredModelDownload`, which App owns so the first-run wizard can show
 * the same progress.
 */
export default function Mask({ modelsNeedDownload, sizeMb, download }) {
  const { t } = useTranslation();
  const {
    modelDownloadLog,
    modelDownloadError,
    overallProgress,
    isClosedByUser,
    setIsClosedByUser,
    retryModelDownload,
  } = download;
  const visible = useOverlaySlot('modelDownload', modelsNeedDownload && !isClosedByUser);

  return (
    <OverlayShell
      open={visible}
      size="lg"
      icon={Download}
      title={t('mask.model_download.title')}
      subtitle={t('mask.model_download.subtitle', { size: sizeMb })}
      footer={(
        <>
          <Button size="md" onClick={() => setIsClosedByUser(true)}>
            {t('mask.model_download.run_in_background')}
          </Button>
          {modelDownloadError && (
            <Button size="md" variant="primary" icon={RotateCcw} onClick={retryModelDownload}>
              {t('mask.model_download.retry')}
            </Button>
          )}
        </>
      )}
    >
      {modelDownloadError ? (
        <Banner tone="error">{t('mask.model_download.failed', { error: modelDownloadError })}</Banner>
      ) : (
        <ProgressBlock label={t('mask.model_download.syncing')} percent={Math.round(overallProgress)} />
      )}
      <LogDisclosure lines={modelDownloadLog} error={Boolean(modelDownloadError)} />
    </OverlayShell>
  );
}
