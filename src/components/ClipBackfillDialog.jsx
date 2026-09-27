import React, { useCallback, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Images } from 'lucide-react';
import { getClipBackfillOffer, setClipBackfillDecision } from '../lib/semantic_api';
import { formatError } from '../lib/errors';
import { usePolling } from '../hooks/usePolling';
import { OverlayShell, useOverlaySlot } from './overlay';
import { Button } from './ui/Button';
import { Banner } from './ui/Banner';

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

  usePolling(async () => {
    const next = await refresh();
    // A recorded answer is final until the settings card changes it. A
    // settled migration with no work is terminal too. `should_ask` alone is
    // not a stop signal: it is also false while the migration is unfinished,
    // and a failed refresh returns no offer at all; both cases must retry.
    return !(
      next?.decision
      || (next?.migration_settled && !next.should_ask && !next.diagnostics_deferred)
    );
  }, { intervalMs: POLL_MS });

  const decide = async (decision) => {
    setSubmitting(decision);
    setError(null);
    try {
      setOffer(await setClipBackfillDecision(decision));
    } catch (err) {
      setError(formatError(err));
    } finally {
      setSubmitting(null);
    }
  };

  const estimate = formatEstimate(t, offer?.estimated_seconds);
  const isOpen = Boolean(offer?.should_ask) && !dismissed;
  const visible = useOverlaySlot('clipBackfill', isOpen);
  if (!visible) return null;

  const busy = submitting !== null;

  return (
    <OverlayShell
      onDismiss={() => setDismissed(true)}
      size="lg"
      icon={Images}
      title={t('clipBackfill.title')}
      footer={(
        <>
          <Button size="md" disabled={busy} loading={submitting === 'declined'} onClick={() => decide('declined')}>
            {t('clipBackfill.decline')}
          </Button>
          <Button size="md" variant="primary" disabled={busy} loading={submitting === 'approved'} onClick={() => decide('approved')}>
            {t('clipBackfill.approve')}
          </Button>
        </>
      )}
    >
      <p className="text-sm leading-relaxed text-ide-text">
        {t('clipBackfill.lead', { count: offer.never_indexed })}
      </p>

      <div className="space-y-1.5 rounded-lg border border-ide-border/60 bg-ide-bg p-3 text-xs">
        <div className="flex justify-between gap-4">
          <span className="text-ide-muted">{t('clipBackfill.missing')}</span>
          <span className="tabular-nums text-ide-text">{offer.never_indexed}</span>
        </div>
        {offer.stalled > 0 && (
          <div className="flex justify-between gap-4">
            <span className="text-ide-muted">{t('clipBackfill.stalled')}</span>
            <span className="tabular-nums text-ide-warning">{offer.stalled}</span>
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
        <div className="space-y-1 text-[11px] leading-relaxed text-ide-muted">
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

      <p className="text-[11px] leading-relaxed text-ide-muted">
        {t('clipBackfill.whenItRuns')}
      </p>

      {error && <Banner tone="error">{error}</Banner>}
    </OverlayShell>
  );
}
