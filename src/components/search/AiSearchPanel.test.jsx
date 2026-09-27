import React from 'react';
import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key, options) => (options?.returnObjects ? ['example question'] : key),
  }),
}));

vi.mock('../../lib/monitor_api', () => ({
  fetchThumbnail: vi.fn(async () => null),
}));

vi.mock('../../lib/settings_api', () => ({
  openSettingsWindow: vi.fn(async () => {}),
}));

import { AiSearchPanel } from './AiSearchPanel';
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
});
