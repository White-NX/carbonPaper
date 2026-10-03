import React from 'react';
import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import RecapProgress from './RecapProgress';

vi.mock('react-i18next', () => ({ useTranslation: () => ({
  t: (key, options) => options ? `${key} ${JSON.stringify(options)}` : key,
  i18n: { language: 'en' },
}) }));
const start = new Date('2026-10-04T08:00:00').getTime();
const attempt = {
  id: 'run:initial', batch_start_ms: start, kind: 'initial', model: 'example-model',
  started_at_ms: 1000, finished_at_ms: 10000, status: 'ready',
  reasoning: 'Thinking about the records', reasoning_chars: 20000, text: '<script>private</script>', text_chars: 24,
  max_output_tokens: 32000, input_tokens: 1000, output_tokens: 1200, reasoning_tokens: 800, error: null,
};
const batch = { start_ms: start, end_ms: start + 14400000, status: 'ready', activities: [{ id: 'a' }], summary: { overview: 'Done' }, attempts: [attempt] };
const periodButton = () => screen.getByRole('button', { name: /08:00–12:00/ });
const stepButton = () => screen.getByRole('button', { name: /recap.progress.attempts.initial/ });
const selectTab = name => fireEvent.click(screen.getByRole('tab', { name: `recap.diagnostics.tabs.${name}` }));
afterEach(() => vi.useRealTimers());

describe('recap generation details', () => {
  it('starts completed periods collapsed and reveals request content only on demand', () => {
    render(<RecapProgress batches={[batch]} />);
    expect(screen.getByRole('status')).toHaveTextContent('recap.diagnostics.ready');
    expect(periodButton()).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByText(attempt.reasoning)).not.toBeInTheDocument();
    expect(screen.queryByText(attempt.model)).not.toBeInTheDocument();
    fireEvent.click(periodButton());
    fireEvent.click(stepButton());
    expect(screen.getByText(attempt.text)).toBeVisible();
    expect(document.querySelector('script')).toBeNull();
    expect(screen.queryByText(attempt.reasoning)).not.toBeInTheDocument();
    selectTab('reasoning');
    expect(screen.getByText(attempt.reasoning)).toBeVisible();
    expect(screen.getByText(/recap.progress.previewLimited/)).toBeVisible();
    selectTab('requestDetails');
    expect(screen.getByText(attempt.model)).toBeVisible();
    expect(screen.getByText(attempt.id)).toBeVisible();
  });

  it('opens a failed period while keeping raw errors inside request information', () => {
    const failed = { ...attempt, status: 'failed', error: 'RECAP_OUTPUT_TRUNCATED' };
    render(<RecapProgress batches={[{ ...batch, status: 'failed', error: failed.error, attempts: [failed] }]} />);
    expect(periodButton()).toHaveAttribute('aria-expanded', 'true');
    expect(screen.queryByText(failed.error, { exact: true })).not.toBeInTheDocument();
    fireEvent.click(stepButton());
    expect(screen.getByRole('alert')).toHaveTextContent('recap.errors.RECAP_OUTPUT_TRUNCATED');
    selectTab('requestDetails');
    expect(screen.getByText(failed.error, { exact: true })).toBeVisible();
  });

  it('distinguishes a missing overview from failed activity generation', () => {
    render(<RecapProgress batches={[{ ...batch, summary: null, summary_error: 'AI_NETWORK_ERROR' }]} />);
    expect(periodButton()).toHaveTextContent('recap.diagnostics.states.summaryFailed');
    expect(screen.getByText('recap.summaryUnavailable')).toBeVisible();
    expect(screen.getByRole('status')).toHaveTextContent('recap.progress.stages.partial');
    expect(screen.getByText(/recap.diagnostics.completedPeriods/)).toHaveTextContent('"count":0');
  });

  it('distinguishes unknown usage from zero and supports keyboard navigation', () => {
    render(<RecapProgress batches={[{ ...batch, attempts: [{ ...attempt, input_tokens: 0, output_tokens: null, reasoning_tokens: null }] }]} />);
    fireEvent.click(periodButton()); fireEvent.click(stepButton());
    fireEvent.keyDown(screen.getByRole('tab', { name: 'recap.diagnostics.tabs.reply' }), { key: 'End' });
    const selected = screen.getByRole('tab', { name: 'recap.diagnostics.tabs.requestDetails' });
    expect(selected).toHaveFocus();
    expect(selected).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByText('0')).toBeVisible();
    expect(screen.getAllByText('recap.progress.unknownUsage')).toHaveLength(2);
    expect(screen.getByRole('tabpanel')).toHaveAttribute('aria-labelledby', selected.id);
  });

  it('keeps an inspected request and selected tab open as generation advances', () => {
    const live = { ...attempt, id: 'live', status: 'running', finished_at_ms: null };
    const progress = { run_id: 'run', stage: 'thinking', started_at_ms: 1000, batch_start_ms: start, total_batches: 2, completed_batches: 0, attempts: [live] };
    const { rerender } = render(<RecapProgress progress={progress} batches={[batch]} />);
    expect(periodButton()).toHaveAttribute('aria-expanded', 'true');
    fireEvent.click(stepButton()); selectTab('reasoning');
    const text = screen.getByText(attempt.reasoning);
    rerender(<RecapProgress batches={[batch]} progress={{ ...progress, version: 2, stage: 'collecting', batch_start_ms: start + 14400000, completed_batches: 1, attempts: [{ ...live, reasoning: 'Updated thinking', finished_at_ms: 11000, status: 'ready' }] }} />);
    expect(periodButton()).toHaveAttribute('aria-expanded', 'true');
    expect(screen.getByRole('tab', { name: 'recap.diagnostics.tabs.reasoning' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByText('Updated thinking')).toBe(text);
    expect(screen.getByRole('progressbar')).toHaveAttribute('aria-valuenow', '50');
  });

  it('respects a manually collapsed active period after text updates', () => {
    const progress = { run_id: 'run', stage: 'thinking', started_at_ms: 1000, batch_start_ms: start, attempts: [{ ...attempt, finished_at_ms: null, status: 'running' }] };
    const { rerender } = render(<RecapProgress progress={progress} batches={[batch]} />);
    fireEvent.click(periodButton());
    rerender(<RecapProgress progress={{ ...progress, version: 2 }} batches={[batch]} />);
    expect(periodButton()).toHaveAttribute('aria-expanded', 'false');
  });

  it.each(['cancelled', 'paused'])('freezes unfinished request durations after the run is %s', stage => {
    vi.useFakeTimers(); vi.setSystemTime(50000);
    const progress = { run_id: 'run', stage, started_at_ms: 1000, finished_at_ms: 12000, batch_start_ms: start, attempts: [{ ...attempt, id: 'stopped', finished_at_ms: null, status: 'running' }] };
    render(<RecapProgress progress={progress} batches={[batch]} />);
    expect(stepButton()).toHaveTextContent(`recap.progress.stages.${stage}`);
    expect(stepButton()).toHaveTextContent('"count":11');
    act(() => vi.advanceTimersByTime(30000));
    expect(stepButton()).toHaveTextContent('"count":11');
    expect(screen.queryByRole('progressbar')).not.toBeInTheDocument();
  });

  it('does not count a failed saved period as successful when all periods were processed', () => {
    render(<RecapProgress batches={[{ ...batch, status: 'failed', error: 'AI_NETWORK_ERROR' }]}
      progress={{ started_at_ms: 1000, finished_at_ms: 12000, stage: 'partial', total_batches: 1, completed_batches: 1, attempts: [] }} />);
    expect(screen.getByText(/recap.diagnostics.completedPeriods/)).toHaveTextContent('"count":0');
    expect(screen.getByRole('status')).toHaveTextContent('recap.progress.stages.partial');
  });

  it('shows indeterminate finalization when activity processing is done but generation continues', () => {
    render(<RecapProgress batches={[batch]} progress={{ run_id: 'run', stage: 'summarizing', started_at_ms: 1000, batch_start_ms: start, total_batches: 1, completed_batches: 1, attempts: [] }} />);
    expect(screen.getByRole('progressbar')).not.toHaveAttribute('aria-valuenow');
    expect(screen.getByRole('progressbar')).toHaveAccessibleName('recap.diagnostics.finishing');
    expect(screen.getByRole('status')).toHaveTextContent('recap.generating');
  });

  it('shows setup errors and an empty history without an empty disclosure', () => {
    const { rerender } = render(<RecapProgress />);
    expect(screen.getByRole('status')).toHaveTextContent('recap.diagnostics.states.noHistory');
    expect(within(screen.getByRole('list')).queryAllByRole('listitem')).toHaveLength(0);
    rerender(<RecapProgress error="AI_NO_PROVIDER" />);
    expect(screen.getByRole('alert')).toHaveTextContent('recap.errors.AI_NO_PROVIDER');
  });
});
