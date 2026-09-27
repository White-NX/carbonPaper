import React, { useEffect, useRef } from 'react';
import { ChevronDown } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { cn } from '../../lib/utils';

/**
 * Raw log lines behind a "details" toggle. The log is for the curious and for
 * bug reports; the progress bar above it is what everyone else reads.
 */
export function LogDisclosure({ lines, error = false, defaultOpen = false, className }) {
  const { t } = useTranslation();
  const logRef = useRef(null);

  useEffect(() => {
    if (logRef.current) logRef.current.scrollTop = logRef.current.scrollHeight;
  }, [lines]);

  if (!lines?.length) return null;

  return (
    <details className={cn('group', className)} open={defaultOpen || undefined}>
      <summary className="flex cursor-pointer list-none items-center gap-1 text-xs text-ide-muted hover:text-ide-text">
        <ChevronDown className="h-3.5 w-3.5 transition-transform group-open:rotate-180" aria-hidden="true" />
        {t('overlay.showDetails')}
      </summary>
      <textarea
        ref={logRef}
        readOnly
        value={lines.join('\n')}
        rows={7}
        className={cn('mt-2 w-full resize-none rounded-md border bg-ide-bg p-3 font-mono text-xs',
          error ? 'border-red-500/40 text-ide-error' : 'border-ide-border text-ide-muted')}
      />
    </details>
  );
}
