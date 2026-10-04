import { describe, expect, it } from 'vitest';
import { recapAttemptStatus, recapDiagnosticOutcome, recapDiagnosticPeriods, recapRequestDuration } from './recap_diagnostics';

const initial = { id: 'initial', batch_start_ms: 1000, kind: 'initial', started_at_ms: 10, finished_at_ms: 20, status: 'ready' };
const summary = { ...initial, id: 'summary', kind: 'summary', started_at_ms: 30, finished_at_ms: 40 };
const batch = { start_ms: 1000, end_ms: 2000, status: 'ready', activities: [{ id: 'a' }], summary: { overview: 'Ready' }, attempts: [initial, summary] };

describe('recap diagnostics state', () => {
  it('retains activity requests when only the summary is retried', () => {
    const retry = { ...summary, id: 'retry', status: 'running' };
    const [period] = recapDiagnosticPeriods([batch], { stage: 'summarizing', batch_start_ms: 1000, attempts: [retry] });
    expect(period.attempts.map(a => a.id)).toEqual(['initial', 'retry']);
    expect(period.status).toBe('summarizing');
  });

  it('replaces obsolete requests during full regeneration without duplicating saved attempts', () => {
    const retry = { ...initial, id: 'new' };
    const progress = { stage: 'ready', finished_at_ms: 100, attempts: [retry] };
    expect(recapDiagnosticPeriods([batch], progress)[0].attempts).toEqual([retry]);
    const saved = { ...batch, attempts: [retry] };
    expect(recapDiagnosticPeriods([saved], progress)[0].attempts).toEqual([retry]);
  });

  it('waits for saved outcomes instead of inferring success from the final request', () => {
    const repair = { ...initial, id: 'repair', kind: 'repair' };
    const progress = { stage: 'ready', finished_at_ms: 100, attempts: [repair] };
    expect(recapDiagnosticPeriods([batch], progress)[0].status).toBe('updating');
    const saved = { ...batch, status: 'partial', attempts: [repair] };
    expect(recapDiagnosticPeriods([saved], progress)[0].status).toBe('partial');
  });

  it('uses final batch outcomes after a successful repair while retaining failed requests', () => {
    const failed = { ...initial, status: 'failed', error: 'RECAP_INVALID_CITATION' };
    const repair = { ...initial, id: 'repair', kind: 'repair', started_at_ms: 25 };
    const [period] = recapDiagnosticPeriods([{ ...batch, attempts: [summary, repair, failed] }], null);
    expect(period.status).toBe('ready');
    expect(period.attempts.map(a => a.id)).toEqual(['initial', 'repair', 'summary']);
    expect(recapDiagnosticOutcome([period])).toBe('ready');
  });

  it('does not call a ready activity batch complete while its summary is missing or failed', () => {
    const [pending] = recapDiagnosticPeriods([{ ...batch, summary: null }], null);
    const [failed] = recapDiagnosticPeriods([{ ...batch, summary_error: 'AI_NETWORK_ERROR' }], null);
    expect(pending.status).toBe('summaryPending');
    expect(failed.status).toBe('summaryFailed');
    expect(recapDiagnosticOutcome([pending])).toBe('partial');
  });

  it('includes the active period before a request or a cached batch exists', () => {
    const [period] = recapDiagnosticPeriods([], { batch_start_ms: 1000, stage: 'screening', attempts: [] });
    expect(period).toMatchObject({ start_ms: 1000, active: true, status: 'screening', attempts: [] });
    expect(period.end_ms).toBeUndefined();
  });

  it('preserves a summary invalidated by corrections after the last completed run', () => {
    const corrected = { ...batch, summary: null, attempts: [initial] };
    const [period] = recapDiagnosticPeriods([corrected], { stage: 'ready', finished_at_ms: 100, attempts: [initial, summary] });
    expect(period.status).toBe('summaryPending');
    expect(recapDiagnosticOutcome([period])).toBe('partial');
  });

  it('uses request time boundaries and never extends archived incomplete requests to now', () => {
    expect(recapRequestDuration(initial, 999, null)).toBe(10);
    const incomplete = { ...initial, finished_at_ms: null, status: 'running' };
    expect(recapRequestDuration(incomplete, 999, { finished_at_ms: 40 })).toBe(30);
    expect(recapRequestDuration(incomplete, 999, null)).toBeNull();
    expect(recapAttemptStatus(incomplete, null)).toBe('interrupted');
    expect(recapAttemptStatus(incomplete, { finished_at_ms: 40, stage: 'paused' })).toBe('paused');
  });
});
