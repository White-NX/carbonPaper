import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { ExternalLink, Loader2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { resumeOfficeDocument } from '../lib/monitor_api';
import {
  getOfficeDocumentApplicationKey,
  getOfficeDocumentIcon,
  getOfficeDocumentKindKey,
} from '../lib/document_ref';

/**
 * A contextual, clickable representation of the Office document associated
 * with a screenshot.  The encrypted locator never reaches this component;
 * the authenticated resume command resolves it on the backend.
 */
export default function OfficeDocumentEntry({
  documentRef,
  screenshotId,
  compact = false,
  className = '',
}) {
  const { t } = useTranslation();
  const [isOpening, setIsOpening] = useState(false);
  const [error, setError] = useState(null);

  const documentKey = `${screenshotId || ''}:${documentRef?.display_name || ''}:${documentRef?.application || ''}:${documentRef?.kind || ''}:${documentRef?.resumable ? '1' : '0'}`;
  useEffect(() => {
    setIsOpening(false);
    setError(null);
  }, [documentKey]);

  const applicationKey = getOfficeDocumentApplicationKey(documentRef?.application);
  const kindKey = getOfficeDocumentKindKey(documentRef?.kind);
  const Icon = getOfficeDocumentIcon(documentRef?.application);
  const displayName = documentRef?.display_name || t('documentSource.unknownFile');
  const applicationLabel = t(`documentSource.applications.${applicationKey}`);
  const kindLabel = t(`documentSource.kinds.${kindKey}`);
  const canResume = Boolean(documentRef?.resumable && Number(screenshotId) > 0);

  const title = useMemo(() => {
    if (canResume) return t('documentSource.openTitle', { name: displayName });
    if (kindKey === 'unsaved') return t('documentSource.unsavedTitle');
    return t('documentSource.unavailableTitle');
  }, [canResume, displayName, kindKey, t]);

  const handleOpen = useCallback(async () => {
    if (!canResume || isOpening) return;
    setIsOpening(true);
    setError(null);
    try {
      await resumeOfficeDocument(Number(screenshotId));
    } catch (nextError) {
      console.error('Failed to open associated Office document', nextError);
      setError(t('documentSource.openFailed', {
        error: nextError?.message || String(nextError),
      }));
    } finally {
      setIsOpening(false);
    }
  }, [canResume, isOpening, screenshotId, t]);

  if (!documentRef) return null;

  const rowClassName = compact
    ? 'flex w-full items-center gap-2 rounded border border-ide-border bg-ide-bg px-2 py-1.5 text-left text-xs transition-colors'
    : 'flex w-full items-start gap-2 rounded p-2 text-left text-xs transition-colors';
  const stateClassName = canResume
    ? 'cursor-pointer hover:bg-ide-hover'
    : 'cursor-not-allowed opacity-60';

  return (
    <div className={`min-w-0 ${className}`}>
      <button
        type="button"
        onClick={handleOpen}
        disabled={!canResume || isOpening}
        className={`${rowClassName} ${stateClassName} group disabled:pointer-events-auto`}
        title={title}
      >
        <Icon className={`mt-0.5 h-3.5 w-3.5 shrink-0 ${canResume ? 'text-ide-accent' : 'text-ide-muted'}`} />
        <span className="min-w-0 flex-1">
          <span className="block truncate font-medium text-ide-text" title={displayName}>
            {displayName}
          </span>
          <span className="mt-0.5 block truncate text-[10px] text-ide-muted">
            {applicationLabel} · {kindLabel}
          </span>
        </span>
        {isOpening ? (
          <Loader2 className="mt-0.5 h-3.5 w-3.5 shrink-0 animate-spin text-ide-accent" />
        ) : (
          <ExternalLink className={`mt-0.5 h-3.5 w-3.5 shrink-0 ${canResume ? 'text-ide-muted group-hover:text-ide-accent' : 'text-ide-muted'}`} />
        )}
      </button>
      {error && (
        <div role="alert" className="mt-1 break-words text-[11px] text-red-400">
          {error}
        </div>
      )}
    </div>
  );
}
