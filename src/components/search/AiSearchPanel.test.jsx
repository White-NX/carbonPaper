import React from 'react';
import { fireEvent, render, screen, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key, options) => (options?.returnObjects ? ['example question'] : `${key}${options ? ':' + JSON.stringify(options) : ''}`),
  }),
}));

vi.mock('../../lib/monitor_api', () => ({
  fetchThumbnail: vi.fn(async () => null),
}));

vi.mock('../../lib/settings_api', () => ({
  openSettingsWindow: vi.fn(async () => {}),
}));

import { AiSearchPanel, Steps, Reasoning } from './AiSearchPanel';
import { openSettingsWindow } from '../../lib/settings_api';

const Header = ({ children, secondaryRow, as: Tag = 'div', onSubmit }) => (
  <Tag onSubmit={onSubmit}>{secondaryRow}{children}</Tag>
);

function controller(overrides = {}) {
  return {
    settings: { providers: [{ id: 'p', name: 'DeepSeek' }], default_provider_id: 'p' },
    provider: { id: 'p', name: 'DeepSeek' },
    status: 'idle',
    question: '',
    answer: '',
    steps: [],
    outcome: null,
    error: null,
    start: vi.fn(),
    cancel: vi.fn(),
    acceptConsent: vi.fn(),
    declineConsent: vi.fn(),
    ...overrides,
  };
}

describe('AiSearchPanel', () => {
  it('describes daily recap lookups and groups them separately from archives', () => {
    const t = (key, options) => `${key}:${options?.date ?? options?.count ?? ''}`;
    render(<Steps steps={[
      { id: 'dates', name: 'get_recap_days', status: 'done', itemCount: 2 },
      { id: 'day', name: 'get_recap_day', arguments: { date: '2026-10-01' }, status: 'done' },
    ]} running={false} t={t} />);
    fireEvent.click(screen.getByRole('button', { name: 'aiSearch.steps.recaps:2' }));
    expect(screen.getByText('aiSearch.steps.recap_days:')).toBeInTheDocument();
    expect(screen.getByText('aiSearch.steps.recap_day:2026-10-01')).toBeInTheDocument();
  });

  it('renders answer tables with interactive citations and aligned cells', () => {
    const onSelectResult = vi.fn();
    const snapshots = [{ id: 42, window_title: 'Paper' }];
    const c = controller({
      status: 'done', question: 'Publication years?',
      answer: '| Paper | Year | Source |\n| --- | ---: | :---: |\n| **RoleLLM** | 2023 | [#42] |',
      outcome: { snapshots },
    });
    render(<AiSearchPanel controller={c} header={Header} onSelectResult={onSelectResult} />);
    const table = screen.getByRole('table');
    expect(within(table).getAllByRole('columnheader')).toHaveLength(3);
    expect(within(table).getByRole('cell', { name: '2023' })).toHaveStyle({ textAlign: 'right' });
    expect(within(table).getByText('RoleLLM').tagName).toBe('STRONG');
    fireEvent.click(within(table).getByRole('button', { name: '#42' }));
    expect(onSelectResult).toHaveBeenCalledWith(expect.objectContaining({ screenshot_id: 42 }));
    expect(table.parentElement).toHaveClass('overflow-x-auto');
  });

  it('submits from the composer while preserving newlines and IME input', () => {
    const c = controller();
    render(<AiSearchPanel controller={c} header={Header} />);
    const input = screen.getByRole('textbox');
    expect(input.tagName).toBe('TEXTAREA');
    expect(input).toHaveFocus();
    fireEvent.change(input, { target: { value: '  Find my notes  ' } });
    fireEvent.keyDown(input, { key: 'Enter', shiftKey: true });
    fireEvent.keyDown(input, { key: 'Enter', isComposing: true });
    expect(c.start).not.toHaveBeenCalled();
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(c.start).toHaveBeenCalledWith('Find my notes');
    expect(input).toHaveValue('');
  });

  it('keeps a follow-up draft when Enter is pressed during generation', () => {
    const c = controller({ status: 'running', question: 'First question' });
    render(<AiSearchPanel controller={c} header={Header} />);
    const input = screen.getByRole('textbox');
    fireEvent.change(input, { target: { value: 'Tell me more' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(c.start).not.toHaveBeenCalled();
    expect(c.cancel).not.toHaveBeenCalled();
    expect(input).toHaveValue('Tell me more');
    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('First question');
    expect(screen.getByRole('button', { name: 'aiSearch.newConversation' })).toBeDisabled();
  });

  it('points to settings when no model service is configured', () => {
    render(<AiSearchPanel controller={controller({ settings: { providers: [] }, provider: null })} header={Header} />);
    fireEvent.click(screen.getByText('aiSearch.setup.button'));
    expect(openSettingsWindow).toHaveBeenCalledWith('ai', 'ai-providers');
  });

  it('starts a search from an example question', () => {
    const c = controller();
    render(<AiSearchPanel controller={c} header={Header} />);
    fireEvent.click(screen.getByText('example question'));
    expect(c.start).toHaveBeenCalledWith('example question');
  });

  it('opens the cited screenshot with the metadata the tools reported', () => {
    const onSelectResult = vi.fn();
    const snapshots = [{ id: 42, window_title: 'Invoice.pdf', timestamp: 1700000000 }];
    const c = controller({
      status: 'done',
      question: 'where is the invoice?',
      answer: 'It was in **Invoice.pdf** [#42]. Unknown [#7].',
      steps: [{ id: 'c1', name: 'search_ocr_text', arguments: { query: 'invoice' }, status: 'done', itemCount: 1, snapshots }],
      outcome: { answer: '', snapshots, stopped_early: false, truncated: false },
    });
    render(<AiSearchPanel controller={c} header={Header} onSelectResult={onSelectResult} />);

    fireEvent.click(screen.getAllByText('#42')[0]);
    expect(onSelectResult).toHaveBeenCalledWith(expect.objectContaining({ screenshot_id: 42, window_title: 'Invoice.pdf' }));
    expect(screen.getByText('#7').closest('button')).toBeDisabled();
  });

  it('turns the submit button into a stop button while running', () => {
    const c = controller({ status: 'running', question: 'q' });
    render(<AiSearchPanel controller={c} header={Header} />);
    fireEvent.click(screen.getByLabelText('aiSearch.stop'));
    expect(c.cancel).toHaveBeenCalled();
  });
  it('shows detailed activity counts and preserves expansion when a run finishes', () => {
    const t = (key, options) => `${key}${options?.count ? ':' + options.count : ''}`;
    const steps = [
      { id: 'a', name: 'search_nl', status: 'done', itemCount: 3 },
      { id: 'b', name: 'search_ocr_text', status: 'done', itemCount: 1 },
      { id: 'c', name: 'get_snapshot_details', arguments: { id: 42 }, status: 'done' },
      { id: 'd', name: 'get_snapshots_by_time_range', status: 'running' },
    ];
    const { rerender } = render(<Steps steps={steps} running t={t} />);
    const toggle = screen.getByRole('button');
    expect(toggle.textContent).toContain('aiSearch.steps.searches:2');
    expect(toggle.textContent).toContain('aiSearch.steps.screenshots:1');
    expect(toggle.textContent).toContain('aiSearch.steps.records:1');
    expect(screen.queryByRole('list')).not.toBeInTheDocument();
    fireEvent.click(toggle);
    expect(screen.getAllByRole('listitem')).toHaveLength(4);
    rerender(<Steps steps={steps} running={false} t={t} />);
    expect(screen.getByRole('button')).toHaveAttribute('aria-expanded', 'true');
  });

  it('limits reasoning previews and expands them on demand', () => {
    render(<Reasoning text="Provider thinking" t={(key) => key} />);
    expect(screen.getByText('Provider thinking')).toHaveClass('line-clamp-3');
    fireEvent.click(screen.getByRole('button'));
    expect(screen.getByText('Provider thinking')).not.toHaveClass('line-clamp-3');
  });

  it('keeps commentary and follow-up citations to earlier screenshots', () => {
    const onSelectResult = vi.fn();
    const c = controller({
      status: 'done', question: 'follow-up', answer: 'Again [#42]',
      rounds: [{ id: 1, text: 'I will check.', reasoning: 'Plan' }, { id: 2, text: 'Again [#42]', reasoning: '' }],
      turns: [{ status: 'done', question: 'first', answer: 'Found it.', steps: [], outcome: { snapshots: [{ id: 42, window_title: 'Invoice' }] } }],
      reset: vi.fn(),
    });
    render(<AiSearchPanel controller={c} header={Header} onSelectResult={onSelectResult} />);
    expect(screen.getByText('I will check.')).not.toBeVisible();
    const turn = screen.getByText('follow-up').closest('article');
    fireEvent.click(within(turn).getByRole('button', { name: 'aiSearch.reasoning' }));
    expect(screen.getByText('I will check.')).toBeVisible();
    fireEvent.click(screen.getAllByText('#42').find((node) => node.closest('button')?.textContent === '#42'));
    expect(onSelectResult).toHaveBeenCalledWith(expect.objectContaining({ screenshot_id: 42 }));
    const input = screen.getByPlaceholderText('aiSearch.followUp');
    fireEvent.change(input, { target: { value: 'more' } });
    fireEvent.click(screen.getByLabelText('aiSearch.send'));
    expect(c.start).toHaveBeenCalledWith('more');
    expect(input).toHaveValue('');
    fireEvent.click(screen.getByText('aiSearch.newConversation'));
    expect(c.reset).toHaveBeenCalled();
  });

  it('collapses all activity on completion with counts and duration while retaining the final answer', () => {
    const c = controller({
      status: 'running', question: 'Find the invoice', answer: 'The final answer.',
      rounds: [
        { id: 1, reasoning: 'Search plan', text: 'I will look for the invoice.' },
        { id: 2, reasoning: 'Check the match', text: 'The final answer.' },
      ],
      steps: [
        { id: 's1', round: 1, name: 'search_nl', status: 'done' },
        { id: 's2', round: 1, name: 'search_ocr_text', status: 'done' },
        { id: 's3', round: 1, name: 'get_snapshot_details', arguments: { id: 42 }, status: 'done' },
      ],
    });
    const { rerender } = render(<AiSearchPanel controller={c} header={Header} />);
    expect(screen.getByText('I will look for the invoice.')).toBeVisible();
    expect(screen.getByText('Check the match')).toBeVisible();
    // The reader can also collapse the activity while output is still streaming.
    const toggle = document.querySelector('button[aria-controls]');
    fireEvent.click(toggle);
    expect(screen.getByText('Search plan')).not.toBeVisible();
    fireEvent.click(toggle);
    rerender(<AiSearchPanel controller={{ ...c, status: 'done', elapsedMs: 65000 }} header={Header} />);
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(toggle).toHaveTextContent('aiSearch.steps.searches:{"count":2}');
    expect(toggle).toHaveTextContent('aiSearch.steps.screenshots:{"count":1}');
    expect(toggle).toHaveTextContent('aiSearch.duration.minutesSeconds:{"minutes":1,"seconds":5}');
    expect(screen.getByText('I will look for the invoice.')).not.toBeVisible();
    expect(screen.getByText('Check the match')).not.toBeVisible();
    expect(screen.getAllByText('The final answer.')).toHaveLength(1);
    expect(screen.getByText('The final answer.')).toBeVisible();
    fireEvent.click(toggle);
    expect(screen.getByText('I will look for the invoice.')).toBeVisible();
    expect(screen.getByText('Check the match')).toBeVisible();
  });

  it.each([
    [1200, 'aiSearch.duration.seconds:{"count":2}'],
    [120000, 'aiSearch.duration.minutes:{"count":2}'],
  ])('summarizes legacy turns and embedded reasoning after %i ms', (elapsedMs, duration) => {
    render(<AiSearchPanel controller={controller({
      status: 'done', question: 'q', answer: '<think>Private preview</think>Visible answer', elapsedMs,
    })} header={Header} />);
    expect(screen.getByText('Private preview')).not.toBeVisible();
    expect(screen.getByText('Visible answer')).toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: duration }));
    expect(screen.getByText('Private preview')).toBeVisible();
  });

  it.each(['cancelled', 'error'])('retains interrupted activity when a run is %s', (status) => {
    render(<AiSearchPanel controller={controller({
      status, question: 'q', answer: 'I will search.', elapsedMs: 2000,
      rounds: [{ id: 1, text: 'I will search.', reasoning: 'Plan' }],
      steps: [{ id: 's1', round: 1, name: 'search_nl', status: status === 'error' ? 'failed' : 'cancelled' }],
    })} header={Header} />);
    expect(screen.getByText('I will search.')).not.toBeVisible();
    fireEvent.click(document.querySelector('button[aria-controls]'));
    expect(screen.getByText('I will search.')).toBeVisible();
  });

  it('highlights only the cited card footer on hover and keyboard focus, including citations beyond twelve', () => {
    const snapshots = Array.from({ length: 13 }, (_, index) => ({ id: index + 1, window_title: `Window ${index + 1}` }));
    const onSelectResult = vi.fn();
    render(<AiSearchPanel controller={controller({
      status: 'done', question: 'q', answer: snapshots.map(({ id }) => `[#${id}]`).join(' '), outcome: { snapshots },
    })} header={Header} onSelectResult={onSelectResult} />);
    const cite = screen.getByRole('button', { name: '#13', exact: true });
    const footer = document.querySelector('[data-source-id="13"] [data-highlighted]');
    fireEvent.mouseEnter(cite);
    expect(footer).toHaveAttribute('data-highlighted', 'true');
    expect(footer).toHaveClass('bg-ide-accent/15', 'border-ide-accent');
    expect(document.querySelectorAll('[data-highlighted="true"]')).toHaveLength(1);
    fireEvent.mouseLeave(cite);
    expect(footer).toHaveAttribute('data-highlighted', 'false');
    fireEvent.focus(cite);
    expect(footer).toHaveAttribute('data-highlighted', 'true');
    fireEvent.mouseEnter(cite);
    fireEvent.mouseLeave(cite);
    expect(footer).toHaveAttribute('data-highlighted', 'true');
    fireEvent.blur(cite);
    expect(footer).toHaveAttribute('data-highlighted', 'false');
    fireEvent.click(cite);
    expect(onSelectResult).toHaveBeenCalledWith(expect.objectContaining({ screenshot_id: 13 }));
  });

});
