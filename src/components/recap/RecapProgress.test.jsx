import React from 'react';
import { render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import RecapProgress from './RecapProgress';

vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key) => key, i18n: { language: 'en' } }) }));
const attempt = {
  id: 'run:initial', batch_start_ms: 1790467200000, kind: 'initial', model: 'example-model',
  started_at_ms: 1000, finished_at_ms: 10000, status: 'failed',
  reasoning: 'Thinking about the records', reasoning_chars: 20000, text: '', text_chars: 0,
  max_output_tokens: 32000, input_tokens: 1000, output_tokens: 32000, reasoning_tokens: 32000,
  error: 'RECAP_OUTPUT_TRUNCATED',
};

describe('recap activity display', () => {
  it('retains thought previews and diagnostics for a failed saved request', () => {
    render(<RecapProgress batches={[{ start_ms: attempt.batch_start_ms, attempts: [attempt] }]} />);
    expect(screen.getByText(attempt.reasoning)).toBeInTheDocument();
    expect(screen.getByText('recap.errors.RECAP_OUTPUT_TRUNCATED')).toBeInTheDocument();
    expect(screen.getByText('recap.progress.previewLimited')).toBeInTheDocument();
    expect(screen.getByText('run:initial')).toBeInTheDocument();
  });
  it('uses current attempts over the saved batch and shows real period progress', () => {
    const live = { ...attempt, id: 'new:initial', reasoning: 'New thought', error: null };
    render(<RecapProgress progress={{ started_at_ms: 1000, finished_at_ms: 12000, stage: 'ready', total_batches: 5, completed_batches: 5, attempts: [live] }} batches={[{ start_ms: attempt.batch_start_ms, attempts: [attempt] }]} />);
    expect(screen.getByRole('progressbar')).toHaveAttribute('value', '5');
    expect(screen.getByText('New thought')).toBeInTheDocument();
    expect(screen.queryByText(attempt.reasoning)).not.toBeInTheDocument();
  });
  it('distinguishes unreported usage from zero and escapes reply previews', () => {
    render(<RecapProgress batches={[{ attempts: [{ ...attempt, input_tokens: 0, output_tokens: null, reasoning_tokens: null, text: '<script>private</script>', error: null }] }]} />);
    expect(screen.getByText('0')).toBeInTheDocument();
    expect(screen.getAllByText('recap.progress.unknownUsage')).toHaveLength(2);
    expect(screen.getByText('<script>private</script>')).toBeInTheDocument();
    expect(document.querySelector('script')).toBeNull();
  });
});
