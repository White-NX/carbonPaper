import React from 'react';
import PropTypes from 'prop-types';
import { useTranslation } from 'react-i18next';
import { Download, RefreshCw, AlertCircle, ArrowUpCircle } from 'lucide-react';
import { OverlayShell, ProgressBlock, useOverlaySlot } from './overlay';
import { Button } from './ui/Button';
import { Banner } from './ui/Banner';

export function UpdateModal({
  isVisible,
  updateInfo,
  downloading,
  downloadProgress,
  downloadError,
  onDownload,
  onLater,
  onClose
}) {
  const { t } = useTranslation();
  const visible = useOverlaySlot('update', Boolean(isVisible && updateInfo));

  if (!visible) return null;

  const { version, body, critical } = updateInfo;
  const phase = downloadProgress?.phase || 'downloading';
  const hasValidTotal = downloadProgress && downloadProgress.contentLength > 0;
  const progressPercent = hasValidTotal
    ? Math.round((downloadProgress.downloaded / downloadProgress.contentLength) * 100)
    : null;
  const isApplying = phase === 'applying';
  const isExtracting = phase === 'extracting';
  const statusLabel = isApplying
    ? t('updateModal.applying')
    : isExtracting
      ? t('updateModal.extracting')
      : t('updateModal.downloading');
  const downloadedMb = !hasValidTotal && downloadProgress
    ? `${(downloadProgress.downloaded / 1024 / 1024).toFixed(1)} MB`
    : null;

  let footer;
  if (downloadError) {
    footer = (
      <>
        <Button size="md" onClick={onClose}>{t('updateModal.later')}</Button>
        <Button size="md" variant="primary" icon={RefreshCw} disabled={downloading} onClick={onDownload}>
          {t('updateModal.retry')}
        </Button>
      </>
    );
  } else if (downloading) {
    footer = <Button size="md" variant="primary" onClick={onClose}>{t('updateModal.hide')}</Button>;
  } else {
    footer = (
      <>
        <Button size="md" onClick={critical ? () => { onDownload(); onClose(); } : onLater}>
          {critical ? t('updateModal.background') : t('updateModal.later')}
        </Button>
        <Button size="md" variant="primary" icon={Download} onClick={onDownload}>
          {t('updateModal.downloadInstall')}
        </Button>
      </>
    );
  }

  return (
    <OverlayShell
      icon={critical ? AlertCircle : ArrowUpCircle}
      tone={critical ? 'danger' : 'accent'}
      title={critical ? t('updateModal.titleCritical') : t('updateModal.title')}
      subtitle={t('updateModal.version', { version })}
      className={critical ? 'max-h-[85vh] border-red-500/50' : 'max-h-[85vh]'}
      bodyClassName="flex flex-col overflow-hidden"
      footer={footer}
    >
      {critical && <Banner tone="error" icon={false} className="shrink-0">{t('updateModal.criticalNotice')}</Banner>}

      <div className="custom-scrollbar min-h-0 flex-1 overflow-y-auto whitespace-pre-wrap rounded-lg border border-ide-border bg-ide-bg p-4 text-sm leading-relaxed text-ide-text/90">
        {body || ''}
      </div>

      {downloadError && (
        <Banner tone="error" className="shrink-0">{t('updateModal.downloadFailed', { error: downloadError })}</Banner>
      )}

      {downloading && (
        <ProgressBlock
          className="shrink-0 pt-2"
          label={statusLabel}
          percent={isApplying || isExtracting ? 100 : progressPercent ?? undefined}
          showCount={!(isApplying || isExtracting)}
          detail={downloadedMb}
        />
      )}
    </OverlayShell>
  );
}

UpdateModal.propTypes = {
  isVisible: PropTypes.bool.isRequired,
  updateInfo: PropTypes.shape({
    version: PropTypes.string,
    body: PropTypes.string,
    critical: PropTypes.bool,
  }),
  downloading: PropTypes.bool,
  downloadProgress: PropTypes.shape({
    downloaded: PropTypes.number,
    contentLength: PropTypes.number,
  }),
  downloadError: PropTypes.string,
  onDownload: PropTypes.func.isRequired,
  onLater: PropTypes.func.isRequired,
  onClose: PropTypes.func.isRequired,
};
