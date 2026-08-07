import React, { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Images, Loader2 } from 'lucide-react';
import { Dialog } from './Dialog';
import { getClipBackfillOffer, setClipBackfillDecision } from '../lib/task_api';

/// Until an answer exists there is something to watch for: the offer only
/// becomes askable once the step-7 copy settles, which can be many minutes
/// after launch. Polling stops for good as soon as the user answers.
const POLL_MS = 15000;

/**
 * Turn an estimate in seconds into something a person can decide against.
 *
 * Deliberately coarse. The underlying model is accurate to a few percent on the
 * machine it was measured on and to nobody knows what on any other, so
 * rendering "3 h 47 min" would claim a precision that is not there. Rounding to
 * hours and quarter-hours says what the decision actually needs: is this a
 * coffee break or an overnight job.
 */
export function formatEstimate(t, seconds) {
  if (!Number.isFinite(seconds) || seconds <= 0) return null;
  if (seconds < 90) return t('clipBackfill.estimateUnderAMinute');
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return t('clipBackfill.estimateMinutes', { minutes });
  const hours = Math.floor(minutes / 60);
  const rest = Math.round((minutes % 60) / 15) * 15;
  if (rest === 0 || rest === 60) {
    return t('clipBackfill.estimateHours', { hours: rest === 60 ? hours + 1 : hours });
  }
  return t('clipBackfill.estimateHoursMinutes', { hours, minutes: rest });
}

/**
 * Asks, once, whether to spend hours of the user's processor re-encoding the
 * screenshots the CLIP migration had no vector to copy.
 *
 * This exists because the alternative shipped first and was worse: the repair
 * scan swept the whole history on its own, so a migration that failed or that
 * met a collection Python had never fully indexed was answered by silently
 * re-encoding everything — hours of vision-transformer work, no progress
 * anywhere a user could see it, and no way to decline. The roadmap's own rule
 * for rebuilds is that they be "explicit, budgeted, and idempotent, never
 * automatic resurrection"; this is the explicit and budgeted part.
 *
 * What it will not do is present a number that alarms without informing. A
 * Chroma id that no live screenshot reproduces is the ordinary consequence of
 * having deleted a screenshot, so any collection with deletion history has
 * plenty; those are reported as skipped, separately, and are not part of what a
 * backfill would fix.
 */
export default function ClipBackfillDialog() {
  const { t } = useTranslation();
  const [offer, setOffer] = useState(null);
  const [submitting, setSubmitting] = useState(null);
  const [error, setError] = useState(null);
  const [dismissed, setDismissed] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const next = await getClipBackfillOffer();
      setOffer(next);
      return next;
    } catch {
      // A locked or not-yet-initialised database is not an error worth showing;
      // the next tick asks again.
      return null;
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    let timer;
    const tick = async () => {
      const next = await refresh();
      if (cancelled) return;
      // A recorded answer is final until the settings card changes it. A
      // settled migration with no work is terminal too. `should_ask` alone is
      // not a stop signal: it is also false while the migration is unfinished,
      // and a failed refresh returns no offer at all; both cases must retry.
      if (
        next?.decision
        || (next?.migration_settled && !next.should_ask && !next.diagnostics_deferred)
      ) return;
      timer = setTimeout(tick, POLL_MS);
    };
    tick();
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [refresh]);

  const decide = async (decision) => {
    setSubmitting(decision);
    setError(null);
    try {
      setOffer(await setClipBackfillDecision(decision));
    } catch (err) {
      setError(typeof err === 'string' ? err : err?.message || String(err));
    } finally {
      setSubmitting(null);
    }
  };

  const estimate = formatEstimate(t, offer?.estimated_seconds);
  const isOpen = Boolean(offer?.should_ask) && !dismissed;
  if (!isOpen) return null;

  const busy = submitting !== null;

  return (
    <Dialog
      isOpen={isOpen}
      onClose={() => setDismissed(true)}
      title={t('clipBackfill.title')}
      maxWidth="max-w-xl"
    >
      <div className="p-4 space-y-4">
        <div className="flex items-start gap-3">
          <div className="w-9 h-9 rounded-lg bg-ide-bg border border-ide-border flex items-center justify-center shrink-0">
            <Images className="w-4 h-4 text-ide-accent" />
          </div>
          <p className="text-sm text-ide-text leading-relaxed">
            {t('clipBackfill.lead', { count: offer.never_indexed })}
          </p>
        </div>

        <div className="rounded-lg border border-ide-border/60 bg-ide-panel/40 p-3 space-y-1.5 text-xs">
          <div className="flex justify-between gap-4">
            <span className="text-ide-muted">{t('clipBackfill.missing')}</span>
            <span className="text-ide-text tabular-nums">{offer.never_indexed}</span>
          </div>
          {offer.stalled > 0 && (
            <div className="flex justify-between gap-4">
              <span className="text-ide-muted">{t('clipBackfill.stalled')}</span>
              <span className="text-ide-warning tabular-nums">{offer.stalled}</span>
            </div>
          )}
          <div className="flex justify-between gap-4">
            <span className="text-ide-muted">{t('clipBackfill.estimateLabel')}</span>
            <span className="text-ide-text">{estimate ?? '—'}</span>
          </div>
        </div>

        {/* The two migration numbers that explain where the gap came from, and
            which must not be read as one. */}
        {(offer.skipped_deleted > 0 || offer.failed_imports > 0) && (
          <div className="text-[11px] text-ide-muted leading-relaxed space-y-1">
            {offer.skipped_deleted > 0 && (
              <p>{t('clipBackfill.skippedDeleted', { count: offer.skipped_deleted })}</p>
            )}
            {offer.failed_imports > 0 && (
              <p className="text-ide-warning-muted">
                {t('clipBackfill.failedImports', { count: offer.failed_imports })}
              </p>
            )}
          </div>
        )}

        <p className="text-[11px] text-ide-muted leading-relaxed">
          {t('clipBackfill.whenItRuns')}
        </p>

        {error && (
          <div className="p-3 bg-red-500/10 border border-red-500/20 rounded text-xs text-red-400">
            {error}
          </div>
        )}

        <div className="flex justify-end gap-2">
          <button
            onClick={() => decide('declined')}
            disabled={busy}
            className="px-4 py-1.5 bg-ide-bg border border-ide-border rounded text-sm hover:bg-ide-hover transition-colors disabled:opacity-50 flex items-center gap-1.5"
          >
            {submitting === 'declined' && <Loader2 className="w-3.5 h-3.5 animate-spin" />}
            {t('clipBackfill.decline')}
          </button>
          <button
            onClick={() => decide('approved')}
            disabled={busy}
            className="px-4 py-1.5 bg-ide-accent text-white rounded text-sm hover:opacity-90 transition-colors disabled:opacity-50 flex items-center gap-1.5"
          >
            {submitting === 'approved' && <Loader2 className="w-3.5 h-3.5 animate-spin" />}
            {t('clipBackfill.approve')}
          </button>
        </div>
      </div>
    </Dialog>
  );
}
