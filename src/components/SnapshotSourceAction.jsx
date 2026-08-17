import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { ExternalLink, Loader2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { resumeOfficeDocument } from '../lib/monitor_api';
import { getSnapshotSourceOptions } from '../lib/snapshot_sources';
import {
  getOfficeDocumentApplicationKey,
  getOfficeDocumentIcon,
  getOfficeDocumentKindKey,
} from '../lib/document_ref';

function getUrlLabel(url) {
  try {
    return new URL(url).hostname || url;
  } catch {
    return url;
  }
}

function SourceOptionIcon({ source }) {
  if (source.kind === 'office') {
    const Icon = getOfficeDocumentIcon(source.documentRef?.application);
    return <Icon className="h-3.5 w-3.5 shrink-0 text-ide-accent" />;
  }
  return <ExternalLink className="h-3.5 w-3.5 shrink-0 text-ide-accent" />;
}

function SourceOptionText({ source, t }) {
  if (source.kind === 'office') {
    const applicationKey = getOfficeDocumentApplicationKey(source.documentRef?.application);
    const kindKey = getOfficeDocumentKindKey(source.documentRef?.kind);
    return (
      <span className="min-w-0 flex-1">
        <span className="block truncate text-left text-xs text-ide-text" title={source.documentRef?.display_name}>
          {source.documentRef?.display_name || t('documentSource.unknownFile')}
        </span>
        <span className="mt-0.5 block truncate text-left text-[10px] text-ide-muted">
          {t(`documentSource.applications.${applicationKey}`)} · {t(`documentSource.kinds.${kindKey}`)}
        </span>
      </span>
    );
  }

  return (
    <span className="min-w-0 flex-1">
      <span className="block truncate text-left text-xs text-ide-text" title={source.url}>
        {getUrlLabel(source.url)}
      </span>
      <span className="mt-0.5 block truncate text-left text-[10px] text-ide-muted" title={source.url}>
        {source.url}
      </span>
    </span>
  );
}

/**
 * One source action for the preview surfaces.  With one source it opens
 * directly; with multiple sources it presents a small chooser instead of
 * silently opening the first OCR URL.
 */
export default function SnapshotSourceAction({
  documentRef,
  screenshotId,
  pageUrl,
  ocrResults,
  onOpenUrl,
  compact = false,
  className = '',
}) {
  const { t } = useTranslation();
  const [menuOpen, setMenuOpen] = useState(false);
  const [isOpening, setIsOpening] = useState(false);
  const [error, setError] = useState(null);

  const sources = useMemo(() => getSnapshotSourceOptions({
    documentRef,
    screenshotId,
    pageUrl,
    ocrResults,
  }), [documentRef, ocrResults, pageUrl, screenshotId]);
  const sourceSignature = sources.map((source) => source.id).join('|');

  useEffect(() => {
    setMenuOpen(false);
    setError(null);
  }, [sourceSignature, screenshotId]);

  const openSource = useCallback(async (source) => {
    if (!source || isOpening) return;
    setMenuOpen(false);
    setIsOpening(true);
    setError(null);
    try {
      if (source.kind === 'office') {
        if (Number(screenshotId) <= 0) throw new Error('Invalid screenshot id');
        await resumeOfficeDocument(Number(screenshotId));
      } else if (onOpenUrl) {
        await onOpenUrl(source.url);
      } else {
        throw new Error('URL opener is unavailable');
      }
    } catch (nextError) {
      console.error('Failed to open associated snapshot source', nextError);
      setError(t('snapshotSource.openFailed', {
        error: nextError?.message || String(nextError),
      }));
    } finally {
      setIsOpening(false);
    }
  }, [isOpening, onOpenUrl, screenshotId, t]);

  const handleButtonClick = useCallback(() => {
    if (isOpening) return;
    if (sources.length === 1) {
      openSource(sources[0]);
    } else {
      setMenuOpen((previous) => !previous);
      setError(null);
    }
  }, [isOpening, openSource, sources]);

  if (sources.length === 0) return null;

  const buttonClassName = compact
    ? 'flex items-center gap-1.5 rounded-full px-3 py-1.5 text-xs text-ide-text transition-colors hover:bg-ide-hover disabled:cursor-wait disabled:opacity-60'
    : 'flex w-full items-center justify-center gap-2 rounded-md border border-ide-accent bg-ide-accent/10 px-3 py-2 text-xs font-medium text-ide-accent transition-colors hover:bg-ide-accent/20 disabled:cursor-wait disabled:opacity-60';
  const errorClassName = compact
    ? 'absolute bottom-full left-1/2 z-50 mb-2 w-max max-w-[min(32rem,calc(100vw-2rem))] -translate-x-1/2 break-words rounded-md border border-red-500/40 bg-ide-panel px-3 py-2 text-xs text-red-400 shadow-xl'
    : 'mt-1 break-words text-[11px] text-red-400';

  return (
    <div className={`relative ${className}`}>
      {menuOpen && (
        <div
          className="fixed inset-0 z-20"
          onClick={() => setMenuOpen(false)}
          aria-hidden="true"
        />
      )}

      {error && (
        <div role="alert" className={errorClassName}>
          {error}
        </div>
      )}

      <button
        type="button"
        onClick={handleButtonClick}
        disabled={isOpening}
        className={buttonClassName}
        title={sources.length > 1 ? t('snapshotSource.chooseTitle') : t('snapshotSource.openTitle')}
        aria-haspopup={sources.length > 1 ? 'menu' : undefined}
        aria-expanded={sources.length > 1 ? menuOpen : undefined}
      >
        {isOpening ? (
          <Loader2 className={compact ? 'h-3.5 w-3.5 animate-spin' : 'h-4 w-4 animate-spin'} />
        ) : (
          <ExternalLink className={compact ? 'h-3.5 w-3.5' : 'h-3.5 w-3.5'} />
        )}
        <span>{isOpening ? t('snapshotSource.opening') : t('snapshotSource.open')}</span>
      </button>

      {menuOpen && (
        <div
          role="menu"
          className={`absolute bottom-full z-40 mb-2 min-w-64 max-w-[min(24rem,calc(100vw-2rem))] overflow-hidden rounded-lg border border-ide-border bg-ide-panel p-1 shadow-xl ${compact ? 'right-0' : 'left-0 right-0'}`}
        >
          {sources.map((source) => (
            <button
              key={source.id}
              type="button"
              role="menuitem"
              onClick={() => openSource(source)}
              className="flex w-full items-center gap-2 rounded-md px-2.5 py-2 text-left transition-colors hover:bg-ide-hover"
            >
              <SourceOptionIcon source={source} />
              <SourceOptionText source={source} t={t} />
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
