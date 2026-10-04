const FINISHED = new Set(['ready', 'failed', 'partial', 'cancelled', 'paused', 'empty']);

export function recapAttemptStatus(attempt, progress) {
  if (!progress && !FINISHED.has(attempt.status)) return 'interrupted';
  if (progress?.finished_at_ms != null && !FINISHED.has(attempt.status)) {
    return ['cancelled', 'paused', 'failed'].includes(progress.stage) ? progress.stage : 'interrupted';
  }
  return attempt.status;
}

export function recapRequestDuration(attempt, now, progress) {
  const end = attempt.finished_at_ms ?? progress?.finished_at_ms ?? (progress ? now : null);
  if (!Number.isFinite(attempt.started_at_ms) || !Number.isFinite(end)) return null;
  return Math.max(0, end - attempt.started_at_ms);
}

function savedStatus(batch) {
  if (!batch) return 'pending';
  if (batch.status === 'failed') return 'failed';
  if (batch.status === 'partial' || batch.error) return 'partial';
  if (batch.summary_error) return 'summaryFailed';
  if (batch.activities?.length && !batch.summary) return 'summaryPending';
  return batch.status || 'pending';
}

/** Keep activity history when only the derived summary is retried. A new
 * activity request replaces both the old activity attempts and their summary.
 * Saved batch outcomes remain authoritative: a successful request can still
 * leave uncovered records or an unfinished summary.
 */
export function recapDiagnosticPeriods(batches, progress) {
  const groups = new Map(batches.map(batch => [batch.start_ms, { ...batch, saved: batch, live: [] }]));
  for (const attempt of progress?.attempts || []) {
    if (!Number.isFinite(attempt.batch_start_ms)) continue;
    if (!groups.has(attempt.batch_start_ms)) groups.set(attempt.batch_start_ms, { start_ms: attempt.batch_start_ms, live: [] });
    groups.get(attempt.batch_start_ms).live.push(attempt);
  }
  const current = progress?.batch_start_ms;
  if (Number.isFinite(current) && !groups.has(current)) groups.set(current, { start_ms: current, live: [] });
  return [...groups.values()].sort((a, b) => a.start_ms - b.start_ms).map(group => {
    const saved = group.saved?.attempts || [];
    const regenerating = group.live.some(attempt => attempt.kind !== 'summary');
    const retained = group.live.length ? (regenerating ? [] : saved.filter(attempt => attempt.kind !== 'summary')) : saved;
    const attempts = [...new Map([...retained, ...group.live].map(attempt => [attempt.id, attempt])).values()]
      .sort((a, b) => a.started_at_ms - b.started_at_ms);
    const active = progress?.finished_at_ms == null && progress != null && current === group.start_ms;
    const unsaved = group.live.some(attempt => !saved.some(old => old.id === attempt.id && old.status === attempt.status));
    let status = savedStatus(group.saved);
    if (active) status = progress.stage;
    else if (unsaved && ['cancelled', 'paused', 'failed'].includes(progress.stage)) status = progress.stage;
    // Corrections can invalidate a saved summary after the last run finished.
    // Preserve that incomplete result instead of waiting forever for old attempts.
    else if (unsaved && ['ready', 'empty', 'pending'].includes(status)) status = 'updating';
    return { ...group, attempts, active, status };
  });
}

export function recapDiagnosticOutcome(periods) {
  if (!periods.length) return 'noHistory';
  if (periods.every(period => ['ready', 'empty'].includes(period.status))) return 'ready';
  if (periods.every(period => period.status === 'failed')) return 'failed';
  if (periods.some(period => ['failed', 'partial', 'summaryFailed', 'summaryPending'].includes(period.status))) return 'partial';
  if (periods.some(period => period.status === 'updating')) return 'updating';
  return 'pending';
}
